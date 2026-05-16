pub mod plan_mode;
mod protocol;
mod runtime;
pub mod subagent;
mod task_runner;
mod tool;

pub use plan_mode::{PlanFile, PlanStep};
pub use protocol::{
    ChatMessage, ChatRequest, ChatResponse, DeltaSink, LlmClient, Role, ToolCall, ToolFunction,
    ToolSpec, expand_file_refs, parse_text_tool_calls, smart_truncate, trim_history_tags,
};
pub use runtime::{AgentRuntime, RuntimeEvent, RuntimeOptions, RuntimeOutcome, RuntimeState};
pub use subagent::{
    ROUND_END_MARKER, SubagentHandle, SubagentRun, SubagentSnapshot, SubagentSpec, SubagentStatus,
    allocate_workdir, append_output, append_round_end, intervene, load_run, read_context,
    scan_runs, snapshot, stop, write_context, write_input, write_reply,
};
pub use task_runner::{TaskEvent, TaskOutcome, TaskRunOpts, run_task};
pub use tool::{StepOutcome, Tool, ToolContext, ToolRegistry, WorkingMemory};
