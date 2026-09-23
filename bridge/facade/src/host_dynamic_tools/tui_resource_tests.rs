//! Matched full-TUI acceptance with a local scripted provider; no paid model calls.
use super::*;
use crate::host_dynamic_tools::{HostDynamicToolService, MODEL_OUTPUT_LIMIT};
use axum::{extract::State, routing::post, Json, Router};
use exomonad_node::command_resources::{CommandResourcePolicy, CommandResources};
use exomonad_node::{
    ProcessInvocation, ProcessMountBoundary, ProcessSupervisorClient, ProcessSupervisorManifest,
    ProcessSupervisorObservation, ServiceEnvironment, TmuxLaunch, TmuxSession,
};
use serde_json::{json, Value};
use std::{collections::BTreeMap, path::PathBuf, sync::Mutex as StdMutex, time::Duration};
use tokio::net::UnixListener;

#[derive(Clone)]
struct Provider {
    requests: Arc<StdMutex<Vec<Value>>>,
    steps: Arc<Vec<Option<String>>>,
    work: PathBuf,
    shell: exomonad_agent::InteractiveShellTools,
}

async fn response(
    State(provider): State<Provider>,
    Json(body): Json<Value>,
) -> impl axum::response::IntoResponse {
    let title = body
        .pointer("/text/format/schema/properties/title")
        .is_some();
    let index = {
        let mut requests = provider.requests.lock().unwrap();
        let index = requests.len();
        if !title {
            requests.push(body);
        }
        index
    };
    if let Some(marker) = match index {
        3 => Some("holder-started"),
        10 => Some("cancel-started"),
        14 => Some("terminal-started"),
        24 => Some("structured-started"),
        28 => Some("structured-interrupt-ready"),
        _ => None,
    } {
        tokio::time::timeout(Duration::from_secs(120), async {
            while !provider.work.join(marker).exists() {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("prior native command must actually start");
    }
    let item = if title {
        json!({"type":"message", "role":"assistant", "id":"title",
            "content":[{"type":"output_text","text":"{\"title\":\"Exercise resource limits\"}"}]})
    } else if index == 0 {
        let args = match provider.shell {
            exomonad_agent::InteractiveShellTools::Native => {
                json!({"cmd":"python3 -c 'a=bytearray(512*1024*1024)'", "yield_time_ms":1000,"max_output_tokens":1000})
            }
            exomonad_agent::InteractiveShellTools::Hosted => {
                json!({"cmd":"python3 -c 'a=bytearray(512*1024*1024)'", "memory_mib":64,"yield_time_ms":30000})
            }
        };
        json!({"type":"function_call", "call_id":"initial-oom", "name":match provider.shell { exomonad_agent::InteractiveShellTools::Native => "exec_command", exomonad_agent::InteractiveShellTools::Hosted => "bash" }, "arguments":args.to_string()})
    } else if index == 40 {
        json!({"type":"function_call", "call_id":"structured-40", "name":"bash",
            "arguments":json!({"cmd":"python3 -c 'import sys; sys.stdout.buffer.write(bytes([255])*9000)'", "memory_mib":64,"yield_time_ms":30000}).to_string()})
    } else if index == 41 || index == 42 {
        let requests = provider.requests.lock().unwrap();
        let output = |call: &str| {
            requests[index]["input"]
                .as_array()
                .unwrap()
                .iter()
                .find(|item| item["call_id"] == call && item["type"] == "function_call_output")
                .and_then(|item| item["output"].as_str())
                .unwrap()
        };
        let session = output("structured-40")
            .split("session_id: ")
            .nth(1)
            .unwrap()
            .split_whitespace()
            .next()
            .unwrap();
        let offset = if index == 41 {
            0
        } else {
            output("structured-41")
                .split("next_offset: ")
                .nth(1)
                .unwrap()
                .trim()
                .parse::<usize>()
                .unwrap()
        };
        json!({"type":"function_call", "call_id":format!("structured-{index}"), "name":"read_output",
            "arguments":json!({"session_id":session,"offset":offset,"max_output_bytes":if index == 41 {1024} else {8192}}).to_string()})
    } else if index == 30 || index == 34 {
        let arguments = if index == 30 {
            json!({"cmd":"wc -c", "stdin":true, "yield_time_ms":0})
        } else {
            json!({"cmd":"printf cancel-ready; exec sleep 30", "yield_time_ms":0})
        };
        json!({"type":"function_call", "call_id":format!("structured-{index}"),
            "name":"bash", "arguments":arguments.to_string()})
    } else if (31..=33).contains(&index) || (35..=38).contains(&index) {
        let origin = if index == 38 {
            27
        } else if index >= 35 {
            34
        } else {
            30
        };
        let requests = provider.requests.lock().unwrap();
        let receipt = requests[index]["input"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| {
                item["call_id"] == format!("structured-{origin}")
                    && item["type"] == "function_call_output"
            })
            .and_then(|item| item["output"].as_str())
            .expect("existing process receipt");
        let session = receipt
            .split("session_id: ")
            .nth(1)
            .unwrap()
            .split_whitespace()
            .next()
            .unwrap();
        let (name, arguments) = match index {
            31 => (
                "write_stdin",
                json!({"session_id":session,"chars":"abc\n","close_stdin":true,"yield_time_ms":30000}),
            ),
            32 | 38 => (
                "write_stdin",
                json!({"session_id":session,"close_stdin":true,"yield_time_ms":0}),
            ),
            37 => ("read_output", json!({"session_id":session})),
            _ => (
                "cancel_command",
                json!({"session_id":session,"yield_time_ms":30000}),
            ),
        };
        json!({"type":"function_call", "call_id":format!("structured-{index}"), "name":name,"arguments":arguments.to_string()})
    } else if index == 20 || index == 21 {
        let script = if index == 20 {
            "cat <<'EOF'\nraw λ $(literal) [bash|data|]\nEOF\nprintf 'raw-stderr\\n' >&2\n"
        } else {
            "printf 'once\\n' >> raw-start-count; printf 'RAW-BEGIN\\n'; head -c 96000 /dev/zero | tr '\\0' x; printf '\\nRAW-END\\n'; printf 'nonzero diagnostic\\n' >&2; exit 7"
        };
        json!({"type":"function_call", "call_id":format!("haskell-{index}"),
            "name":"bash", "arguments":json!({"cmd":script}).to_string()})
    } else if index == 22 {
        let requests = provider.requests.lock().unwrap();
        let receipt = requests[index]["input"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| item["call_id"] == "haskell-21" && item["type"] == "function_call_output")
            .and_then(|item| item["output"].as_str())
            .expect("structured Bash output in provider history");
        let binding = receipt
            .lines()
            .find_map(|line| {
                line.strip_prefix("retained as ")?
                    .strip_suffix(" :: Cmd.Job")
            })
            .expect("large command installs an actual job binding");
        json!({"type":"custom_tool_call", "call_id":"haskell-22",
            "name":"haskell",
            "input":format!("rawPage <- Cmd.output {binding}\n(T.length (Cmd.pageText rawPage), T.take 9 (Cmd.pageText rawPage))")})
    } else if index == 23 {
        json!({"type":"function_call", "call_id":"structured-23",
            "name":"bash", "arguments":json!({"cmd":"printf once >> structured-count; touch structured-started; read -r line; printf 'structured:%s\\n' \"$line\"; printf 'structured-error\\n' >&2; exit 7", "tty":true,"memory_mib":64,"yield_time_ms":0}).to_string()})
    } else if (24..=26).contains(&index) {
        let requests = provider.requests.lock().unwrap();
        let receipt = requests[index]["input"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| {
                item["call_id"] == "structured-23" && item["type"] == "function_call_output"
            })
            .and_then(|item| item["output"].as_str())
            .expect("structured command receipt");
        let session = receipt
            .split("session_id: ")
            .nth(1)
            .unwrap()
            .split_whitespace()
            .next()
            .unwrap();
        let (name, arguments) = if index == 24 {
            (
                "write_stdin",
                json!({"session_id":session,"chars":"hello\n","yield_time_ms":30000}),
            )
        } else {
            (
                "read_output",
                json!({"session_id":session,"stream":if index == 25 {"Stdout"} else {"Stderr"}}),
            )
        };
        json!({"type":"function_call","call_id":format!("structured-{index}"),
            "name":name,"arguments":arguments.to_string()})
    } else if index == 27 {
        json!({"type":"function_call", "call_id":"structured-27",
            "name":"bash", "arguments":json!({"cmd":"trap 'exit 42' INT; touch structured-interrupt-ready; while :; do sleep 1; done", "tty":true,"memory_mib":64,"yield_time_ms":0}).to_string()})
    } else if index == 28 {
        let requests = provider.requests.lock().unwrap();
        let receipt = requests[index]["input"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| {
                item["call_id"] == "structured-27" && item["type"] == "function_call_output"
            })
            .and_then(|item| item["output"].as_str())
            .unwrap();
        let session = receipt
            .split("session_id: ")
            .nth(1)
            .unwrap()
            .split_whitespace()
            .next()
            .unwrap();
        json!({"type":"function_call", "call_id":"structured-28", "name":"write_stdin",
            "arguments":json!({"session_id":session,"chars":"\u{3}","yield_time_ms":30000}).to_string()})
    } else if index == 18 {
        let requests = provider.requests.lock().unwrap();
        let receipt = requests[index]["input"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| {
                item["call_id"] == "haskell-17" && item["type"] == "custom_tool_call_output"
            })
            .and_then(|item| item["output"].as_str())
            .expect("foreground handoff in next provider request");
        let binding = receipt
            .lines()
            .find_map(|line| line.strip_suffix(" :: Cmd.Job"))
            .expect("installed recovery binding");
        assert!(binding.starts_with("job") && binding[3..].chars().all(|c| c.is_ascii_digit()));
        std::fs::write(provider.work.join("release-foreground"), "release").unwrap();
        json!({"type":"custom_tool_call", "call_id":"haskell-18",
            "name":"haskell",
            "input":format!("recovered <- Cmd.await {binding}\nCmd.stdout recovered")})
    } else if let Some(Some(source)) = provider.steps.get(index) {
        json!({"type":"custom_tool_call", "call_id":format!("haskell-{index}"),
            "name":"haskell", "input":source})
    } else {
        json!({"type":"message", "role":"assistant", "id":format!("message-{index}"),
            "content":[{"type":"output_text","text":format!("fixture-turn-{index}-done")}]})
    };
    let events = [
        json!({"type":"response.created","response":{"id":format!("response-{index}")}}),
        json!({"type":"response.output_item.done","item":item}),
        json!({"type":"response.completed","response":{"id":format!("response-{index}"),
            "usage":{"input_tokens":1,"output_tokens":1,"total_tokens":2}}}),
    ];
    let stream = events
        .iter()
        .map(|event| {
            format!(
                "event: {}\ndata: {event}\n\n",
                event["type"].as_str().unwrap()
            )
        })
        .collect::<String>();
    (
        [
            ("content-type", "text/event-stream"),
            ("connection", "close"),
        ],
        stream,
    )
}

struct NativeFixture {
    session: String,
    process: Option<ProcessSupervisorClient>,
}
impl Drop for NativeFixture {
    fn drop(&mut self) {
        if let Some(mut process) = self.process.take() {
            let _ = process.stop(Duration::from_secs(10));
            let _ = process.finalize(Duration::from_secs(10));
        }
        let _ = std::process::Command::new("tmux")
            .args(["kill-session", "-t", &self.session])
            .status();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires delegated cgroups, EXOMONAD_RESOURCE_CODEX_BIN and EXOMONAD_RESOURCE_HOST_BIN"]
async fn full_tui_survives_command_oom_and_accepts_steering() {
    run_shell_fixture(exomonad_agent::InteractiveShellTools::Hosted).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires delegated cgroups, EXOMONAD_RESOURCE_CODEX_BIN and EXOMONAD_RESOURCE_HOST_BIN"]
async fn full_tui_native_shell_oom_remains_isolated() {
    run_shell_fixture(exomonad_agent::InteractiveShellTools::Native).await;
}

async fn run_shell_fixture(shell: exomonad_agent::InteractiveShellTools) {
    use std::os::unix::fs::PermissionsExt;
    let native = PathBuf::from(
        std::env::var_os("EXOMONAD_RESOURCE_CODEX_BIN").expect("matched native executable"),
    );
    let host_binary = PathBuf::from(
        std::env::var_os("EXOMONAD_RESOURCE_HOST_BIN").expect("matched Exomonad executable"),
    );
    assert!(native.is_absolute() && host_binary.is_absolute());
    let owner = CommandResources::delegated(CommandResourcePolicy {
        general_bytes: 512 * 1024 * 1024,
        protected_bytes: 256 * 1024 * 1024,
        swap_max_bytes: 0,
        ..Default::default()
    })
    .unwrap();
    let temp = tempfile::tempdir().unwrap();
    std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let home = temp.path().join("codex-home");
    let work = temp.path().join("work");
    let private = temp.path().join("supervisor");
    for path in [&home, &work, &private] {
        std::fs::create_dir(path).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    assert!(std::process::Command::new("git")
        .args(["init", "--quiet"])
        .arg(&work)
        .status()
        .unwrap()
        .success());
    let skill = work.join(".exomonad/workspace/skills/exomonad-command");
    std::fs::create_dir_all(&skill).unwrap();
    std::fs::write(
        skill.join("SKILL.md"),
        include_str!("../../../../.exomonad/workspace/skills/exomonad-command/SKILL.md"),
    )
    .unwrap();
    std::fs::create_dir_all(work.join(".agents/skills")).unwrap();
    std::os::unix::fs::symlink(
        "../../.exomonad/workspace/skills/exomonad-command",
        work.join(".agents/skills/exomonad-command"),
    )
    .unwrap();
    let plan_dir = work.join("plans/parallel-dogfood");
    std::fs::create_dir_all(plan_dir.join("next-wave")).unwrap();
    // Frozen snapshot of three real plan documents (a since-pruned campaign
    // record), kept here only as realistic multi-file markdown content for
    // the presentation-budget fixture below; their originals are gone from
    // `plans/` and git history is the archive.
    let resume = r#"# Historical resume record: sleep and applications

This file preserves the source selection and settled decisions used to resume the
resident-sleep and interactive-applications campaign. It is no longer an active
resume instruction. Both implementations are accepted on unified Tidepool main,
paired with native Codex `d0e5fd48e0`. The
[main integration record](../../interactive-applications/main-integration.md) owns
the matched source, evidence and limitations.

Do not recreate the sleep or applications leads from these refs. A later campaign
must choose a new scope and launch from then-current unified main. Git preserves
the earlier actors' source and handoffs; it does not recreate live TUI or Haskell
handles, authority, or checked build state.

## Preserved source provenance

The campaign began from R7 coordinator
`0c1fb83f2285775cb16b212ce2e152bbe9a07374`, containing applications handoff
`8eda2640b5047786f5dcf2af8b7eae9760e5e767`, engine handoff
`0c1ff8d9995dddfc30756a2d26d5278749571826`, and fixture baseline
`6bef6363d95ef5c9bb4749cfd5304c89767d9cb7`. Its original native applications
candidate was `d84cda697a8dac2842bec09dbd7562a3fab4c926`.

Those hashes explain provenance only. Applications and sleep were reconciled with
main rather than preserving every historical patch. Engine M0–M5 and the bounded
M6 seam remain preserved on their own refs; general M6/M7 work was excluded from
the applications and sleep integration. Structured engine introspection remains
part of that preserved engine work, not an unfinished obligation in this release.

There are no external TPLR consumers. The campaign used a coordinated format
cutover and required matched extractor/runtime checks against Exomonad usage. The
only available native platform was x86_64; historical evidence must not be widened
into an aarch64 acceptance claim.

## Reusable operating guidance

For a future recursive Sol campaign, start from one explicit main and package
selection. Give each substantial owner a real integration responsibility and fork
related work after shared contracts are concrete. Related children should inherit
the useful completed reasoning prefix and current bound source; choose fresh
context for unrelated mechanisms or independent review. Keep effort stable across
related Sol forks unless the new scope gives a reason to change it.

Use Haskell collectors for mechanical progress and terminal routing. Send model
turns only useful checkpoints, changed decisions, failures, or final outcomes.
Retain uncertain receipts and exact source evidence. External supervision can own
build, cache, disk, usage, and harness observation while product owners run the
decisive product checks. Do not hot-change a running package or treat a completed
build snapshot as authority for different source.
"#;
    let readme = r#"# Resident sleep and applications wrapup: campaign record

This directory records the completed implementation campaign. Resident sleep and
interactive applications are accepted together on unified Tidepool main, paired
with native Codex `d0e5fd48e0`. Source and check evidence is recorded in the
[applications main integration record](../../interactive-applications/main-integration.md).

[Ready](ready.md), [resume](resume.md), and [commission](commission.md) preserve
the launch selection, checkpoints, and allocation used by this campaign. They are
historical evidence, not authority to launch or recommission the completed sleep
and applications branches. Engine work remains preserved and excluded from this
integration. A later campaign requires a new chosen scope and launch record.
Use the [new RSI commissioning template](new-rsi-commission.md) for an Astra
planning partner and Sol execution tree driven by newly agreed human ideas.

The [resident sleep PRD](../../next/resident-sleep.md), applications
[mechanism router](../../interactive-applications/README.md), and retained engine
[map](engine.md) remain design and provenance references. They do not describe
unfinished lane commissioning.

## Reusable recursive planning guidance

Each substantial assignment should contain:

- A concrete integrated outcome, relevant plan sections and source/file ownership.
- The shared decisions children need, and what minimum wiring makes them usable.
- Independent child outcomes, their own likely decomposition, and the work the
  parent retains. Name actual dependencies and the joins that unlock later work.
- Which reasoning prefix children inherit, what they read afterward, and the
  declared Astra decision/repair slots. Default related work to inherited context
  and live bound source; choose fresh context explicitly where useful.

Plan the first two implementation levels concretely where source supports it.
For distant work, name contracts and outcomes, not speculative file-level tickets.
Ask leads to explain or challenge the decomposition once, then correct material
misunderstandings and let them implement. Existing accepted decisions and saved
scaffolds should shorten this checkpoint; do not regenerate historical plans.

## Package context at the fork

For example, a native-admission owner first establishes operation identity,
transaction boundaries and failure semantics. Fork queue/store implementation and
host-contract fixtures from that completed reasoning; give each a short assignment
and links to its files. The parent implements dispatch integration while they work.
Do not first read every queue bug and host fixture into the common prefix. On the
next cycle, incorporate returned source and summarize changed contracts before
forking dependent work: children inherit a snapshot, not later parent knowledge.

Keep the authored document tree equally small: shared decisions in the execution
contract, branch plans beside their owner's work, deeper sections only where a
subtree needs them. A child packet names outcome, ownership, shared contract,
relevant references and decisive check. A return names commit, behavior, checks
and remaining uncertainty. Link detailed evidence rather than reciting the log.

## Execute continuously

A substantial Sol owner implements shared decisions and difficult joins, forks
independent work while its reasoning is useful, then integrates and repeats.
Once the minimum shared contract works, children can implement and extend owning
fixtures concurrently. A complete subsystem or broad green battery is not a
prerequisite for every fork. Conversely, parallelism does not authorize siblings
to invent incompatible signatures, custody protocols or root/layout rules.

Use a dependency graph, not globally synchronized rounds. One subtree can start
its next local cycle while another repairs its previous result. Review useful
integrated boundaries and consequential risks; reuse reviewers for repairs.
Keep independent engineering moving while a review is pending. Do not build a
standing reviewer/manager hierarchy solely to create more actors.

If a substantial node is heading into prolonged independent investigations or
multiple owning mechanisms, split that work before unrelated debugging fills its
context. If it cannot split, name the concrete coupling in one sentence and keep
working; this is judgment, not a quota or a new approval gate. No headcount cap.

Astra can plan the graph and check Sol's execution understanding once, then idle
without routine progress subscriptions. Sol owns implementation, integration and
ordinary choices. Fresh Astra consultations own bounded hard decisions or repairs
and return directly to the requesting Sol. Haskell handles mechanical collection,
cursor advancement and routing; use .exomonad/plans/operating.md. Keep routine
evidence out of Attention and use compact projections at decision boundaries.
Return compact commits/checks/gates; exact transport and source
incorporation still matter without a paragraph of acknowledgment history.

## Historical inputs and external supervision

Earlier execution-contract.md, planner-review.md and restart-review.md describe
older launches. Their actor handles, release messages, source pins and frozen-helper
workarounds remain evidence only. The applications integration record
supersedes them as the release source. Historical check results are never fresh
integrated evidence.

In a future campaign, external supervision can handle build/cache/usage/disk
monitoring and harness issues. Product workers run checks with the supplied
toolchain and explicit candidate selection; they do not start environment
archaeology or hot-patch the running harness.
Keep ordinary TUIs, one frozen canonical `.exomonad`, and fixed running tools during
any future campaign. Platform gaps must be reported honestly. There are no external
TPLR consumers; coordinate format cutovers and validate Exomonad usage before changing
the selected main or running package.
"#;
    let planner = r#"# Initial Astra planner: make this tree effective

You are the Exomonad-managed planning root, working with the human in your ordinary
Codex TUI. The external setup conversation only supervises the harness. Both
product designs already exist; focus your substantial reasoning on executing them
well, rather than rewriting them or repeating the product interview.

Start with next-wave/README.md and its selected task maps/PRD. Read final checkpoint
evidence supplied by the launch, then deeper mechanisms only for consequential
dependencies or uncertainties. Preserve the full product acceptance.

## Decisions to make before commissioning

- What must each shared context establish before forking? Which reasoning should
  descendants inherit, and which tasks need fresh selected contexts? Separate
  source dependencies from context ancestry. Avoid loading both designs in every
  ancestor; fork before unrelated debugging consumes the useful common prefix.
- What goes in each node's small task packet, linked reference material and upward
  result? Arrange progressive discovery. Keep event handling, routine integration
  and bookkeeping with Sol; reserve your attention for high-leverage decisions.
- Where does Sol have enough shared structure to implement independently? Refine
  the lane maps' Astra slots around actual hard semantic decisions. Give experts
  bounded evidence-rich tasks; do not create standing Astra managers or kill
  useful in-flight work at arbitrary token limits.
- Which scaffold unlocks each ready frontier, and where must results consolidate
  before the next fork? Plan the near frontier precisely and later waves by
  dependencies/outcomes. Preserve local discretion and useful parallelism.

Name useful second-level implementation branches, not only a milestone per lead.
A substantial parent retains shared engineering and integration while its children
advance independent mechanisms. Explain concrete coupling when a large task must
stay serial; do not create additional relay managers to make the tree look deeper.

Record the compact decisions in `execution-contract.md` here, with deeper branch
information linked only where needed. Ask the human about consequential direction
changes; the existing goals and two-lane implementation are already authorized.

## Commission and review inside Exomonad

The selected package loads `Project.Types`, `Project.Work`, `Project.Plan`,
`Project.Actors`, `Project.Routing` and `Project.Observe` unqualified. `Task` is a type, not a
module: its source accessor is `taskSource :: Task -> Text`. Use the supplied
signatures and recipe; query types only when a concrete missing fact blocks work.

```haskell
Project.Plan.componentLeadFrom
  :: BranchLabel -> WorktreeSeed -> Task -> Branch CodingEffects Task Delivery
Project.Work.projectPrompt :: Text -> Text
Project.Work.taskContext :: Task -> Text
```

After authoring the plan, bind `task :: Task` to the coordinator's assignment,
with its exact source, plan path and fork group. The following is executable
resident Haskell once that assignment exists; it launches one Sol Medium
coordinator and retains its progress/reply collector. Names are local bindings,
not extra roles or required workflow stages.

```haskell
import qualified Project.Plan as Plan
import qualified Project.Work as Work
let Right coordinatorLabel = branchLabel "coordinator"
let coordinatorBranch = withInstructions (Work.projectPrompt "coordinator") $ withEffort Medium $ withContext (selected Work.taskContext) $ Plan.componentLeadFrom coordinatorLabel projectHead task
(coordinator, coordinatorProgress) <- unfold (taskGroup task) (childWithProgress @WorkProgress @Delivery coordinatorBranch)
owner <- actorContext
review <- followWork [("coordinator", forkedResponse coordinator, coordinatorProgress)] (notifyWork owner (workMessage deliverySummary))
```

The coordinator commissions both Sol leads and consolidates their initial
execution proposals. `review` has type
`ActorHandle (WorkActor Delivery)`; it receives ordered
progress and final replies without rearming. Inspect it only when needed:

```haskell
proposal <- readWork review
```

Read the referenced proposal artifacts. Send specific corrections and authority
to proceed through `updateRequest (forkedResponse coordinator) correction`,
where `correction :: Text` contains your actual decision. Retain the returned
`Either ReplyError RequestUpdate`; presentation is not checked incorporation.
Do not issue a second delivery request behind its pending first request.

After the initial planning corrections are incorporated and Sol owns execution,
retire only this planner's review collector:

```haskell
reviewExit <- finishWork review
```

The coordinator and its tree continue independently. Remain idle for human
steering or explicit planning work. Hard technical questions go directly from
Sol to fresh selected Astra consultations; routine progress stays with Sol.
"#;
    let skill_text = std::fs::read_to_string(skill.join("SKILL.md")).unwrap();
    let skill_description = skill_text
        .lines()
        .find_map(|line| line.strip_prefix("description: "))
        .unwrap();
    // Exercise the gap between the old native history allowance and hosted cap.
    let padding = 60_000usize
        .checked_sub(skill_text.len() + resume.len() + readme.len() + planner.len() + 100)
        .expect("four-file fixture must fit its presentation budget");
    let readme = format!(
        "{readme}\nREAD-BEGIN\n{}\nREAD-MIDDLE\n{}\nREAD-END\n",
        "a".repeat(padding / 2),
        "b".repeat(padding - padding / 2)
    );
    std::fs::write(plan_dir.join("next-wave/resume.md"), resume).unwrap();
    std::fs::write(plan_dir.join("next-wave/README.md"), &readme).unwrap();
    std::fs::write(plan_dir.join("planner.md"), planner).unwrap();
    let expected_read = format!("{skill_text}{resume}{readme}{planner}");
    let mut campaign = test_campaign::TestCampaign::start().await;
    let actor = campaign.actor.identity();
    let actor_key = format!("{}-{}", actor.id.0, actor.incarnation.0);
    let client = exomonad_node::command_resources::CommandResourceClient::local(owner.clone());
    let snippets: Vec<_> = include_str!("tui_commands.hs")
        .split("-- fixture-step\n")
        .map(str::to_owned)
        .collect();
    let mut steps = vec![None, None];
    steps.extend(snippets[..3].iter().cloned().map(Some));
    steps.push(None);
    steps.extend(snippets[3..].iter().cloned().map(Some));
    steps.push(None);
    steps.extend(
        include_str!("tui_foreground_commands.hs")
            .split("-- fixture-step\n")
            .map(|source| Some(source.to_owned())),
    );
    steps.extend([None, None]);
    let provider = Provider {
        requests: Arc::new(StdMutex::new(Vec::new())),
        steps: Arc::new(steps),
        work: work.clone(),
        shell,
    };
    let tcp = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = tcp.local_addr().unwrap();
    let app = Router::new()
        .route("/v1/responses", post(response))
        .with_state(provider.clone());
    let provider_task = tokio::spawn(async move { axum::serve(tcp, app).await.unwrap() });
    std::fs::write(
        home.join("config.toml"),
        format!(
            r#"
model = "gpt-6-sol"
model_provider = "fixture"
approval_policy = "never"
sandbox_mode = "danger-full-access"
tool_output_token_limit = 16384
[features]
shell_tool = {}
[model_providers.fixture]
name = "Fixture"
base_url = "http://{address}/v1"
wire_api = "responses"
requires_openai_auth = false
supports_websockets = false
[projects.{}]
trust_level = "trusted"
"#,
            shell == exomonad_agent::InteractiveShellTools::Native,
            toml::Value::String(work.display().to_string())
        ),
    )
    .unwrap();
    let socket = temp.path().join("host.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let binding_path = temp.path().join("binding.json");
    let service = HostDynamicToolService::new(
        match shell {
            exomonad_agent::InteractiveShellTools::Hosted => {
                campaign.root_installation.policy.clone()
            }
            exomonad_agent::InteractiveShellTools::Native => {
                crate::host_dynamic_tools::tests::endpoint()
            }
        },
        binding_path.clone(),
        None,
    )
    .unwrap()
    .with_command_resources(Some((client.clone(), actor_key.clone())));
    let control = service.control();
    let server = tokio::spawn(service.serve(listener));
    let environment = BTreeMap::from([
        ("CODEX_HOME".into(), home.display().to_string()),
        ("CODEX_WORKSPACE_SNAPSHOTS".into(), "1".into()),
        (
            "CODEX_COMMAND_RESOURCE_SOCKET".into(),
            socket.display().to_string(),
        ),
        (
            "CODEX_COMMAND_WRITER_CGROUP".into(),
            owner
                .actor_directory(&actor_key)
                .unwrap()
                .display()
                .to_string(),
        ),
        ("TERM".into(), "xterm-256color".into()),
    ]);
    let bubblewrap = std::process::Command::new("which")
        .arg("bwrap")
        .output()
        .unwrap();
    assert!(bubblewrap.status.success());
    let manifest = ProcessSupervisorManifest::new(
        "tui-test".into(),
        "p".repeat(64),
        "r".repeat(64),
        private,
        PathBuf::from(String::from_utf8(bubblewrap.stdout).unwrap().trim()),
        ProcessMountBoundary::new(&work, [work.clone()], [work.clone()]).unwrap(),
        ProcessInvocation {
            program: native.display().to_string(),
            args: vec![
                "--no-alt-screen".into(),
                "--disable".into(),
                "code_mode".into(),
                "--disable".into(),
                "code_mode_only".into(),
                "--host-dynamic-tools-socket".into(),
                socket.display().to_string(),
                "-C".into(),
                work.display().to_string(),
                "fixture bootstrap".into(),
            ],
        },
        ServiceEnvironment {
            set: environment.clone(),
            unset: Default::default(),
        },
    )
    .unwrap();
    let supervisor_socket = manifest.socket_path();
    let manifest_path = manifest.write_new().unwrap();
    let session = format!("exomonad-resource-test-{}", uuid::Uuid::new_v4().simple());
    let tmux = TmuxSession::new(&session).unwrap();
    let mut fixture = NativeFixture {
        session,
        process: None,
    };
    let slice = exomonad_node::systemd_slice::SystemdSlice::default();
    slice.inspect().await.unwrap();
    slice
        .current_membership()
        .expect("test resource owner must share the swarm budget");
    let launch = slice.scope(slice.verified_command(
        &host_binary,
        ProcessInvocation {
            program: host_binary.display().to_string(),
            args: vec![
                "process-supervisor".into(),
                "--manifest".into(),
                manifest_path.display().to_string(),
            ],
        },
    ));
    let pane = tmux
        .create(&TmuxLaunch {
            window_name: "native".into(),
            cwd: work.clone(),
            program: launch.program,
            args: launch.args,
            environment,
            unset_environment: Default::default(),
        })
        .await
        .unwrap();
    tmux.retain_pane_on_exit(&pane).await.unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        while !supervisor_socket.exists() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    let (process, _) = ProcessSupervisorClient::pair(
        supervisor_socket,
        "tui-test".into(),
        "p".repeat(64),
        Duration::from_secs(10),
    )
    .unwrap();
    fixture.process = Some(process);
    let process = fixture.process.as_mut().unwrap();
    assert_eq!(
        process.prepare(Duration::from_secs(10)).unwrap(),
        ProcessSupervisorObservation::Blocked
    );
    assert_eq!(
        process.pin(Duration::from_secs(10)).unwrap(),
        ProcessSupervisorObservation::Pinned
    );
    assert_eq!(
        process.release(Duration::from_secs(10)).unwrap(),
        ProcessSupervisorObservation::Released
    );
    let native_backend = exomonad_agent::native_interactive_backend(
        exomonad_agent::native_interactive_agent_from_parts(native, "command acceptance".into())
            .unwrap(),
    );
    let backend_task = tokio::spawn(async move {
        while let Some(deployment) = campaign.deployments.recv().await {
            if let LocalResidentDeployment::CommandBackend(request) = deployment {
                assert_eq!(request.owner, actor);
                let thread = exomonad_agent::read_interactive_binding(&binding_path)
                    .await
                    .unwrap();
                request.supply(Ok(Arc::new(commands::NativeCommandBackend::new(
                    native_backend.clone(),
                    thread,
                    client.clone(),
                    actor,
                ))));
            }
        }
    });
    let phases: &[usize] = match shell {
        exomonad_agent::InteractiveShellTools::Hosted => &[2, 6, 16, 20, 30, 40, 44],
        exomonad_agent::InteractiveShellTools::Native => &[2],
    };
    for &expected in phases {
        let reached = tokio::time::timeout(
            Duration::from_secs(if expected == 2 { 90 } else { 600 }),
            async {
                while provider.requests.lock().unwrap().len() < expected {
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            },
        )
        .await;
        eprintln!(
            "TUI fixture: reached {} of {expected} provider requests",
            provider.requests.lock().unwrap().len()
        );
        if reached.is_err() {
            let captured = std::process::Command::new("tmux")
                .args(["capture-pane", "-p", "-t", pane.as_str(), "-S", "-100"])
                .output()
                .unwrap();
            panic!(
                "native fixture timed out: {}",
                String::from_utf8_lossy(&captured.stdout)
            );
        }
        let requests = provider.requests.lock().unwrap().clone();
        let output = |index: usize| {
            let call = if index == 1 {
                "initial-oom".to_owned()
            } else {
                format!("haskell-{}", index - 1)
            };
            requests[index]["input"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|item| {
                    item["call_id"] == call
                        && matches!(
                            item["type"].as_str(),
                            Some("function_call_output" | "custom_tool_call_output")
                        )
                })
                .map(|item| {
                    item["output"]
                        .as_str()
                        .expect("textual command result")
                        .to_owned()
                })
                .collect::<Vec<_>>()
                .join("\n")
        };
        match expected {
            2 => {
                let oom = match shell {
                    exomonad_agent::InteractiveShellTools::Native => "resource limit",
                    exomonad_agent::InteractiveShellTools::Hosted => "CommandOutOfMemory",
                };
                assert!(output(1).contains(oom), "{}", output(1));
                assert!(
                    requests[0]["input"].to_string().contains(skill_description),
                    "command skill must appear in native skill discovery"
                );
                let tools = &requests[0]["input"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|item| item["type"] == "additional_tools")
                    .expect("native provider request advertises tools")["tools"];
                let flat = tools
                    .as_array()
                    .expect("tool list")
                    .iter()
                    .find(|tool| tool["type"] == "namespace" && tool["name"] == "functions")
                    .expect("default tools share Codex's functions group")["tools"]
                    .as_array()
                    .expect("default tool list");
                let shell_name = match shell {
                    exomonad_agent::InteractiveShellTools::Native => "exec_command",
                    exomonad_agent::InteractiveShellTools::Hosted => "bash",
                };
                for name in [shell_name, "write_stdin", "haskell", "apply_patch"] {
                    assert_eq!(
                        flat.iter().filter(|tool| tool["name"] == name).count(),
                        1,
                        "{tools}"
                    );
                }
                if shell == exomonad_agent::InteractiveShellTools::Hosted {
                    assert!(!flat.iter().any(|tool| tool["name"] == "exec_command"));
                    for name in ["read_output", "cancel_command"] {
                        assert_eq!(
                            flat.iter().filter(|tool| tool["name"] == name).count(),
                            1,
                            "{tools}"
                        );
                    }
                    let exec = flat.iter().find(|tool| tool["name"] == "bash").unwrap();
                    assert!(
                        exec.to_string().contains("memory_mib"),
                        "hosted schema required: {exec}"
                    );
                    assert!(
                        !exec.to_string().contains("sandbox_permissions"),
                        "native schema leaked: {exec}"
                    );
                }
                assert!(!tools.to_string().contains("exomonad_actor"), "{tools}");
                assert!(
                    !flat
                        .iter()
                        .any(|tool| tool["name"] == "exec" || tool["name"] == "shell_command"),
                    "{tools}"
                );
            }
            6 => {
                assert!(output(4).contains("CommandQueued"), "{}", output(4));
                assert!(output(5).contains("protected-slot-usable"), "{}", output(5));
                assert!(!work.join("release-holder").exists());
                std::fs::write(work.join("release-holder"), "release").unwrap();
            }
            16 => {
                let size = std::process::Command::new("tmux")
                    .args([
                        "display-message",
                        "-p",
                        "-t",
                        pane.as_str(),
                        "#{pane_height} #{pane_width}",
                    ])
                    .output()
                    .unwrap();
                assert!(size.status.success());
                assert_eq!(
                    std::fs::read_to_string(work.join("terminal-size"))
                        .unwrap()
                        .trim(),
                    String::from_utf8(size.stdout).unwrap().trim(),
                    "PTY inherits the owning TUI dimensions"
                );
                for (index, expected) in [
                    (7, "admitted-after-release"),
                    (8, "input-closed"),
                    (9, "CommandOutOfMemory"),
                    (11, "CommandCancelled"),
                    (12, "TAIL-MARKER"),
                    (13, "completion-once"),
                    (15, "terminal:hello"),
                ] {
                    assert!(
                        output(index).contains(expected),
                        "step {index}: {}",
                        output(index)
                    );
                }
                assert!(output(12).contains("page-contiguous"), "{}", output(12));
                assert!(!output(12).contains("retention loss"), "{}", output(12));
            }
            20 => {
                assert!(
                    output(17).contains(&expected_read),
                    "four-file read lost content in provider history"
                );
                let handoff = output(18);
                assert!(handoff.contains(" :: Cmd.Job"), "{handoff}");
                assert!(
                    handoff.contains("subsequent statements did not run"),
                    "{handoff}"
                );
                assert!(handoff.contains("foreground-started"), "{handoff}");
                assert!(!work.join("forbidden-suffix").exists());
                assert_eq!(
                    std::fs::read_to_string(work.join("foreground-start-count")).unwrap(),
                    "once\n"
                );
                assert!(output(19).contains("foreground-finished"), "{}", output(19));
                for index in [17, 18, 19] {
                    assert!(output(index).len() <= MODEL_OUTPUT_LIMIT);
                }
            }
            30 => {
                assert!(
                    output(21).contains("raw λ $(literal) [bash|data|]"),
                    "{}",
                    output(21)
                );
                assert!(output(21).contains("raw-stderr"), "{}", output(21));
                assert!(
                    output(21).contains(" :: Cmd.Job"),
                    "short calls retain a composable job binding"
                );
                let large = output(22);
                assert!(large.len() <= 32 * 1024, "{}", large.len());
                for expected in [
                    "RAW-BEGIN",
                    "RAW-END",
                    "CommandExited 7",
                    "nonzero diagnostic",
                    " :: Cmd.Job",
                ] {
                    assert!(large.contains(expected), "missing {expected}: {large}");
                }
                assert!(output(23).contains("RAW-BEGIN"), "{}", output(23));
                let native_output = |index: usize| {
                    requests[index]["input"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .find(|item| {
                            item["call_id"] == format!("structured-{}", index - 1)
                                && item["type"] == "function_call_output"
                        })
                        .and_then(|item| item["output"].as_str())
                        .unwrap()
                        .to_owned()
                };
                assert!(
                    native_output(25).contains("CommandExited 7"),
                    "{}",
                    native_output(25)
                );
                assert!(
                    native_output(26).contains("structured:hello"),
                    "{}",
                    native_output(26)
                );
                assert!(
                    native_output(29).contains("CommandExited 42"),
                    "{}",
                    native_output(29)
                );
                assert_eq!(
                    std::fs::read_to_string(work.join("structured-count")).unwrap(),
                    "once"
                );
                assert_eq!(
                    std::fs::read_to_string(work.join("raw-start-count")).unwrap(),
                    "once\n"
                );
            }
            40 => {
                let receipt = |index: usize| {
                    requests[index]["input"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .find(|item| {
                            item["call_id"] == format!("structured-{}", index - 1)
                                && item["type"] == "function_call_output"
                        })
                        .and_then(|item| item["output"].as_str())
                        .unwrap()
                };
                assert!(receipt(32).contains("CommandExited 0"), "{}", receipt(32));
                assert!(receipt(32).contains("\n4"), "{}", receipt(32));
                assert!(receipt(33).contains("Stdin is closed"), "{}", receipt(33));
                assert!(
                    receipt(34).contains("CommandExited 0"),
                    "finished outcome changed: {}",
                    receipt(34)
                );
                for index in [36, 37] {
                    assert!(
                        receipt(index).contains("CommandCancelled"),
                        "{}",
                        receipt(index)
                    );
                }
                assert!(receipt(38).contains("bytes"), "{}", receipt(38));
                assert!(receipt(39).contains("PTY"), "{}", receipt(39));
                assert!(
                    receipt(39).contains("input not submitted"),
                    "{}",
                    receipt(39)
                );
                for index in [32, 34, 36, 37] {
                    assert!(
                        receipt(index).contains("cleanup: clean"),
                        "{}",
                        receipt(index)
                    );
                }
            }
            44 => {
                let receipt = |call: &str| {
                    requests[43]["input"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .find(|item| {
                            item["call_id"] == call && item["type"] == "function_call_output"
                        })
                        .and_then(|item| item["output"].as_str())
                        .unwrap()
                };
                let first = receipt("structured-41");
                let next = receipt("structured-42");
                assert!(first.len() <= 1024, "{} bytes: {first}", first.len());
                assert!(next.len() <= 8192, "{} bytes", next.len());
                let position = |text: &str| {
                    text.split("next_offset: ")
                        .nth(1)
                        .unwrap()
                        .trim()
                        .parse::<usize>()
                        .unwrap()
                };
                let end = position(first);
                assert_eq!(end, first.matches('�').count());
                assert!(next.contains(&format!("bytes {end}–")), "{next}");
                assert_eq!(position(next) - end, next.matches('�').count());
            }
            _ => unreachable!(),
        }
        assert!(!tmux.pane_status(&pane).await.unwrap().unwrap().dead);
        if Some(&expected) != phases.last() {
            assert!(std::process::Command::new("tmux")
                .args(["send-keys", "-t", pane.as_str(), "-l", "continue fixture"])
                .status()
                .unwrap()
                .success());
            tokio::time::sleep(Duration::from_millis(200)).await;
            assert!(std::process::Command::new("tmux")
                .args(["send-keys", "-t", pane.as_str(), "Enter"])
                .status()
                .unwrap()
                .success());
        }
    }
    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
    backend_task.abort();
    drop(fixture);
    control.drain();
    server.await.unwrap().unwrap();
    provider_task.abort();
}
