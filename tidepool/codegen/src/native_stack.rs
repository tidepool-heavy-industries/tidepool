//! Current-thread stack admission shared by native entry and root snapshots.

use crate::{
    gc::frame_walker::{FrameWalkError, StackBounds},
    host_fns::RuntimeError,
    machine_state::MachineState,
};
use std::{marker::PhantomData, rc::Rc, thread::ThreadId};

/// Current-thread stack bounds issued only by the supported platform owner.
/// It is neither transferable nor reconstructible from numerical bounds.
pub(crate) struct NativeStackMapping {
    low: usize,
    high: usize,
    thread: ThreadId,
    _thread_bound: PhantomData<Rc<()>>,
    #[cfg(test)]
    fault: std::cell::Cell<u8>,
}

impl NativeStackMapping {
    const UNWIND_RESERVE: usize = 64 * 1024;

    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    fn current() -> Result<Self, RuntimeError> {
        let mut attributes = std::mem::MaybeUninit::<libc::pthread_attr_t>::uninit();
        // Successful getattr initializes the current thread's attributes;
        // destroy runs exactly once before any following return.
        let (address, size, guard) = unsafe {
            if libc::pthread_getattr_np(libc::pthread_self(), attributes.as_mut_ptr()) != 0 {
                return Err(RuntimeError::StackOverflow);
            }
            let attributes = attributes.assume_init();
            let mut address = std::ptr::null_mut();
            let mut size = 0;
            let mut guard = 0;
            let stack_result = libc::pthread_attr_getstack(&attributes, &mut address, &mut size);
            let guard_result = libc::pthread_attr_getguardsize(&attributes, &mut guard);
            let mut attributes = attributes;
            libc::pthread_attr_destroy(&mut attributes);
            if stack_result != 0 || guard_result != 0 || address.is_null() || size == 0 {
                return Err(RuntimeError::StackOverflow);
            }
            (address as usize, size, guard)
        };
        let high = address
            .checked_add(size)
            .ok_or(RuntimeError::StackOverflow)?;
        let low = address
            .checked_add(guard)
            .filter(|low| *low < high)
            .ok_or(RuntimeError::StackOverflow)?;
        Ok(Self {
            low,
            high,
            thread: std::thread::current().id(),
            _thread_bound: PhantomData,
            #[cfg(test)]
            fault: std::cell::Cell::new(0),
        })
    }

    #[cfg(not(all(target_os = "linux", target_arch = "x86_64")))]
    fn current() -> Result<Self, RuntimeError> {
        Err(RuntimeError::StackOverflow)
    }

    fn check_thread(&self) -> Result<(), FrameWalkError> {
        #[cfg(test)]
        if self.fault.get() == 1 {
            return Err(FrameWalkError::StackThreadMismatch);
        }
        if self.thread != std::thread::current().id() {
            return Err(FrameWalkError::StackThreadMismatch);
        }
        Ok(())
    }

    fn limit(&self, frame_reserve: usize) -> Result<*const u8, RuntimeError> {
        self.check_thread()
            .map_err(|_| RuntimeError::StackOverflow)?;
        let limit = self
            .low
            .checked_add(Self::UNWIND_RESERVE)
            .and_then(|low| low.checked_add(frame_reserve))
            .filter(|limit| *limit < self.high)
            .ok_or(RuntimeError::StackOverflow)?;
        Ok(limit as *const u8)
    }

    fn ensure_current_frame_reserve(&self, reserve: usize) -> Result<(), RuntimeError> {
        let limit = self.limit(reserve)? as usize;
        let pointer = current_stack_pointer()?;
        if pointer < limit || pointer >= self.high {
            return Err(RuntimeError::StackOverflow);
        }
        Ok(())
    }

    pub(crate) fn walk_bounds(&self, low: usize) -> Result<StackBounds, FrameWalkError> {
        self.check_thread()?;
        #[cfg(test)]
        if self.fault.get() == 2 {
            return Err(FrameWalkError::InvalidStackMapping {
                address: low,
                low: 0,
                high: 1,
            });
        }
        if low < self.low || low >= self.high {
            return Err(FrameWalkError::InvalidStackMapping {
                address: low,
                low: self.low,
                high: self.high,
            });
        }
        Ok(StackBounds::new(low, self.high))
    }
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
fn current_stack_pointer() -> Result<usize, RuntimeError> {
    let pointer: usize;
    // Reading RSP touches no memory and runs before the platform adapter.
    unsafe {
        std::arch::asm!("mov {}, rsp", out(reg) pointer, options(nomem, nostack, preserves_flags));
    }
    Ok(pointer)
}

#[cfg(not(all(target_os = "linux", target_arch = "x86_64")))]
fn current_stack_pointer() -> Result<usize, RuntimeError> {
    Err(RuntimeError::StackOverflow)
}

enum MachineOwner<'machine> {
    Borrowed(&'machine MachineState),
    Owned(Rc<MachineState>),
}

impl MachineOwner<'_> {
    fn get(&self) -> &MachineState {
        match self {
            Self::Borrowed(machine) => machine,
            Self::Owned(machine) => machine,
        }
    }
}

/// One native phase. Nested entry/forcing borrows its existing mapping;
/// the final scoped receipt clears its weak machine association. No scope
/// escapes a movable session's synchronous native call, and an owned
/// invocation remains thread-bound.
pub(crate) struct NativeStackScope<'machine> {
    machine: MachineOwner<'machine>,
    mapping: Rc<NativeStackMapping>,
}

impl<'machine> NativeStackScope<'machine> {
    pub(crate) fn borrowed(machine: &'machine MachineState) -> Result<Self, RuntimeError> {
        Self::enter(MachineOwner::Borrowed(machine))
    }

    pub(crate) fn owned(
        machine: Rc<MachineState>,
    ) -> Result<NativeStackScope<'static>, RuntimeError> {
        NativeStackScope::enter(MachineOwner::Owned(machine))
    }

    fn enter(machine: MachineOwner<'machine>) -> Result<Self, RuntimeError> {
        let mapping = {
            let state = machine.get();
            let mut active = state.native_stack.borrow_mut();
            if let Some(mapping) = active.as_ref().and_then(std::rc::Weak::upgrade) {
                mapping
                    .check_thread()
                    .map_err(|_| RuntimeError::StackOverflow)?;
                mapping
            } else {
                #[cfg(test)]
                {
                    state
                        .native_stack_queries
                        .set(state.native_stack_queries.get() + 1);
                    if state.fail_next_native_stack_query.replace(false) {
                        return Err(RuntimeError::StackOverflow);
                    }
                }
                let mapping = Rc::new(NativeStackMapping::current()?);
                *active = Some(Rc::downgrade(&mapping));
                mapping
            }
        };
        Ok(Self { machine, mapping })
    }

    pub(crate) fn admit_entry(
        &self,
        maximum_frame: usize,
    ) -> Result<NativeEntryAdmission<'_>, RuntimeError> {
        let reserve = maximum_frame
            .checked_mul(2)
            .ok_or(RuntimeError::StackOverflow)?;
        self.ensure_current_frame_reserve(reserve)?;
        let limit = self.limit_with_frame_reserve(maximum_frame)?;
        Ok(NativeEntryAdmission {
            _scope: self,
            limit,
        })
    }

    pub(crate) fn limit_with_frame_reserve(
        &self,
        reserve: usize,
    ) -> Result<*const u8, RuntimeError> {
        self.mapping.limit(reserve)
    }

    pub(crate) fn ensure_current_frame_reserve(&self, reserve: usize) -> Result<(), RuntimeError> {
        self.mapping.ensure_current_frame_reserve(reserve)
    }
}

impl Drop for NativeStackScope<'_> {
    fn drop(&mut self) {
        if Rc::strong_count(&self.mapping) == 1 {
            self.machine.get().native_stack.borrow_mut().take();
        }
    }
}

/// An entry's checked frame budget borrowed from its current-thread scope.
/// Execution consumes this evidence rather than re-querying after custody moves.
pub(crate) struct NativeEntryAdmission<'scope> {
    _scope: &'scope NativeStackScope<'scope>,
    limit: *const u8,
}

impl NativeEntryAdmission<'_> {
    pub(crate) fn limit(&self) -> *const u8 {
        self.limit
    }
}

static_assertions::assert_not_impl_any!(NativeStackMapping: Clone, Copy, Send, Sync);
static_assertions::assert_not_impl_any!(NativeStackScope<'static>: Send, Sync);
static_assertions::assert_not_impl_any!(NativeEntryAdmission<'static>: Send, Sync);

/// Reentrant generated-code forcing must reuse its admitted phase. Missing
/// ownership is an integrity failure; insufficient headroom is recoverable.
pub(crate) fn ensure_active_frame_reserve(
    machine: &MachineState,
    reserve: usize,
) -> Result<(), RuntimeError> {
    let active = machine.native_stack.borrow();
    let mapping = active.as_ref().and_then(std::rc::Weak::upgrade).ok_or(
        RuntimeError::IncompleteRootSnapshot(FrameWalkError::StackMappingUnavailable),
    )?;
    mapping
        .check_thread()
        .map_err(RuntimeError::IncompleteRootSnapshot)?;
    mapping.ensure_current_frame_reserve(reserve)
}

#[cfg(all(test, target_os = "linux", target_arch = "x86_64"))]
pub(crate) mod tests {
    use super::*;

    #[test]
    fn scopes_share_one_query_and_clear_after_error_or_unwind() {
        let machine = MachineState::new();
        {
            let outer = NativeStackScope::borrowed(&machine).unwrap();
            let inner = NativeStackScope::borrowed(&machine).unwrap();
            assert_eq!(machine.native_stack_queries.get(), 1);
            inner.admit_entry(0).unwrap();
            assert!(matches!(
                outer.admit_entry(usize::MAX),
                Err(RuntimeError::StackOverflow)
            ));
            drop(inner);
            assert!(machine.native_stack.borrow().is_some());
        }
        assert!(machine.native_stack.borrow().is_none());
        // The inner receipt remains authentic even if its sibling ends first.
        let outer = NativeStackScope::borrowed(&machine).unwrap();
        let inner = NativeStackScope::borrowed(&machine).unwrap();
        drop(outer);
        inner.admit_entry(0).unwrap();
        assert!(machine.native_stack.borrow().is_some());
        drop(inner);
        assert!(machine.native_stack.borrow().is_none());
        let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _scope = NativeStackScope::borrowed(&machine).unwrap();
            panic!("unwind after stack admission");
        }));
        assert!(panic.is_err());
        assert!(machine.native_stack.borrow().is_none());
        machine.fail_next_native_stack_query.set(true);
        assert!(matches!(
            NativeStackScope::borrowed(&machine),
            Err(RuntimeError::StackOverflow)
        ));
        assert!(machine.native_stack.borrow().is_none());
        assert_eq!(machine.native_stack_queries.get(), 4);
        let scope = NativeStackScope::borrowed(&machine).unwrap();
        scope.admit_entry(0).unwrap();
    }

    #[test]
    fn real_mapping_rejects_overflow_and_wrong_thread_and_outside_addresses() {
        let mapping = NativeStackMapping::current().unwrap();
        let pointer = current_stack_pointer().unwrap();
        assert!(mapping.walk_bounds(pointer).is_ok());
        assert!(matches!(
            mapping.limit(usize::MAX),
            Err(RuntimeError::StackOverflow)
        ));
        assert!(matches!(
            mapping.walk_bounds(mapping.low - 1),
            Err(FrameWalkError::InvalidStackMapping { .. })
        ));
        assert!(matches!(
            mapping.walk_bounds(mapping.high),
            Err(FrameWalkError::InvalidStackMapping { .. })
        ));
        let mut mapping = mapping;
        mapping.thread = std::thread::spawn(|| std::thread::current().id())
            .join()
            .unwrap();
        assert_eq!(
            mapping.walk_bounds(pointer).unwrap_err(),
            FrameWalkError::StackThreadMismatch
        );
    }

    #[test]
    fn current_small_stack_refuses_reserved_entry_budget() {
        std::thread::Builder::new()
            .stack_size(64 * 1024)
            .spawn(|| {
                let machine = MachineState::new();
                let scope = NativeStackScope::borrowed(&machine).unwrap();
                assert!(matches!(
                    scope.admit_entry(0),
                    Err(RuntimeError::StackOverflow)
                ));
                drop(scope);
                assert!(machine.native_stack.borrow().is_none());
            })
            .unwrap()
            .join()
            .unwrap();
    }

    // Fault controls exercise GC's actual admission boundary without granting
    // production callers an authority constructor from arbitrary numbers.
    pub(crate) fn invalidate_mapping(machine: &MachineState, wrong_thread: bool) {
        let mapping = machine
            .native_stack
            .borrow()
            .as_ref()
            .unwrap()
            .upgrade()
            .unwrap();
        mapping.fault.set(if wrong_thread { 1 } else { 2 });
    }
}
