//! dataToTagSmall# has exactly LiftedRef -> Int64. Call the existing generated
//! prepared_enter through its Tail ABI first, branching on its status before
//! using its managed result. Declare the result to stack maps, then invoke a
//! noncollecting host which reads MachineState::prepared_constructor_tag into
//! a caller-owned scalar output slot. Never derive the result from pointer tag
//! bits, force in Rust, or publish a scalar after a failed entry/inspection.
//!
//! Acceptance: a lazy constructor whose body collects yields its zero-based
//! family tag; an argument raising/cancelling publishes no tag and settles;
//! functions and wrong pointer evidence fail typed. Descriptor tag7 is evidence
//! in this heap, not a function arity or a complete constructor identity.
