use std::path::PathBuf;

use anyhow::Result;
use serde_json::{Value, json};

use crate::{
    ChatMessage, ChatRequest, ChatResponse, LlmClient, ToolCall, ToolContext, ToolRegistry,
    WorkingMemory, parse_text_tool_calls, smart_truncate, trim_history_tags,
};

#[derive(Clone, Debug)]
pub struct RuntimeOptions {
    pub max_turns: usize,
    pub context_char_budget: usize,
    pub cwd: PathBuf,
    pub gars_home: PathBuf,
    pub verbose: bool,
    pub allowed_tools: Option<std::collections::BTreeSet<String>>,
}

impl Default for RuntimeOptions {
    fn default() -> Self {
        Self {
            max_turns: 70,
            context_char_budget: 180_000,
            cwd: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            gars_home: PathBuf::new(),
            verbose: true,
            allowed_tools: None,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct RuntimeState {
    pub history_info: Vec<String>,
    pub working: WorkingMemory,
    pub messages: Vec<ChatMessage>,
    pub current_turn: usize,
}

#[derive(Clone, Debug)]
pub enum RuntimeEvent {
    TurnStarted(usize),
    AssistantText(String),
    ToolStarted { name: String, args: Value },
    ToolFinished { name: String, data: Option<Value> },
    Warning(String),
}

#[derive(Clone, Debug, Default)]
pub struct RuntimeOutcome {
    pub result: String,
    pub final_response: Option<ChatResponse>,
    pub exit_data: Option<Value>,
}

pub struct AgentRuntime<C: LlmClient> {
    pub client: C,
    pub tools: ToolRegistry,
    pub system_prompt: String,
    pub options: RuntimeOptions,
    pub state: RuntimeState,
}

impl<C: LlmClient> AgentRuntime<C> {
    pub fn new(
        client: C,
        tools: ToolRegistry,
        system_prompt: impl Into<String>,
        options: RuntimeOptions,
    ) -> Self {
        Self {
            client,
            tools,
            system_prompt: system_prompt.into(),
            options,
            state: RuntimeState::default(),
        }
    }

    /// Like `run_once` but pipes per-token text deltas to `on_delta` for the
    /// first assistant turn. Tool execution / subsequent turns still run in
    /// non-streaming mode for v0.4.
    pub async fn run_once_stream<F>(
        &mut self,
        user_input: &str,
        emit: F,
        on_delta: crate::DeltaSink<'_>,
    ) -> Result<RuntimeOutcome>
    where
        F: FnMut(RuntimeEvent) + Send,
    {
        self.run_inner(user_input, emit, Some(on_delta)).await
    }

    pub async fn run_once<F>(&mut self, user_input: &str, emit: F) -> Result<RuntimeOutcome>
    where
        F: FnMut(RuntimeEvent) + Send,
    {
        self.run_inner(user_input, emit, None).await
    }

    async fn run_inner<F>(
        &mut self,
        user_input: &str,
        mut emit: F,
        mut delta_sink: Option<crate::DeltaSink<'_>>,
    ) -> Result<RuntimeOutcome>
    where
        F: FnMut(RuntimeEvent) + Send,
    {
        self.state.history_info.push(format!(
            "[USER]: {}",
            smart_truncate(user_input.replace('\n', " "), 200)
        ));
        self.state.messages.push(ChatMessage::user(user_input));

        let mut next_user_prompt = user_input.to_string();
        let mut last_response = None;
        for turn in 1..=self.options.max_turns {
            self.state.current_turn = turn;
            emit(RuntimeEvent::TurnStarted(turn));
            self.trim_context();

            let mut specs = self.tools.specs();
            if let Some(allowed) = &self.options.allowed_tools {
                specs.retain(|s| allowed.contains(&s.function.name));
            }
            let request = ChatRequest {
                system: self.system_prompt.clone(),
                messages: self.state.messages.clone(),
                tools: specs,
            };
            let mut response = if let Some(sink) = delta_sink.take() {
                self.client.chat_stream(request, sink).await?
            } else {
                self.client.chat(request).await?
            };
            let (fallback_calls, cleaned) = parse_text_tool_calls(&response.content);
            if response.tool_calls.is_empty() && !fallback_calls.is_empty() {
                response.tool_calls = fallback_calls;
                response.content = cleaned;
            }
            if !response.content.trim().is_empty() {
                emit(RuntimeEvent::AssistantText(response.content.clone()));
            }
            self.record_summary(&response);

            if response.tool_calls.is_empty() {
                self.state
                    .messages
                    .push(ChatMessage::assistant(response.content.clone()));
                last_response = Some(response);
                return Ok(RuntimeOutcome {
                    result: "CURRENT_TASK_DONE".to_string(),
                    final_response: last_response,
                    exit_data: None,
                });
            }

            self.state
                .messages
                .push(ChatMessage::assistant(response.content.clone()));

            let tool_calls = response.tool_calls.clone();
            let tool_count = tool_calls.len();
            let mut tool_results = Vec::new();
            let mut next_prompts = Vec::new();
            let mut exit_data = None;

            for (index, call) in tool_calls.iter().enumerate() {
                let mut ctx = self.make_tool_context(index, tool_count);
                emit(RuntimeEvent::ToolStarted {
                    name: call.name.clone(),
                    args: call.arguments.clone(),
                });
                let outcome = self.dispatch_tool(call, &mut ctx).await;
                self.state.working = ctx.working;
                self.state.history_info = ctx.history_info;

                match outcome {
                    Ok(outcome) => {
                        emit(RuntimeEvent::ToolFinished {
                            name: call.name.clone(),
                            data: outcome.data.clone(),
                        });
                        if outcome.should_exit {
                            exit_data = outcome.data;
                            break;
                        }
                        if let Some(data) = outcome.data {
                            tool_results.push(json!({
                                "tool_use_id": call.id,
                                "name": call.name,
                                "content": data,
                            }));
                        }
                        if let Some(prompt) = outcome.next_prompt
                            && !prompt.trim().is_empty()
                        {
                            next_prompts.push(prompt);
                        }
                    }
                    Err(err) => {
                        let msg = format!("Tool {} failed: {err:#}", call.name);
                        emit(RuntimeEvent::Warning(msg.clone()));
                        tool_results.push(json!({
                            "tool_use_id": call.id,
                            "name": call.name,
                            "content": { "status": "error", "msg": msg },
                        }));
                        next_prompts
                            .push(self.make_tool_context(index, tool_count).anchor_prompt());
                    }
                }
            }

            if let Some(exit_data) = exit_data {
                last_response = Some(response);
                return Ok(RuntimeOutcome {
                    result: "EXITED".to_string(),
                    final_response: last_response,
                    exit_data: Some(exit_data),
                });
            }

            if next_prompts.is_empty() {
                last_response = Some(response);
                return Ok(RuntimeOutcome {
                    result: "CURRENT_TASK_DONE".to_string(),
                    final_response: last_response,
                    exit_data: None,
                });
            }

            next_user_prompt = format!(
                "<tool_result>{}</tool_result>\n{}",
                serde_json::to_string(&tool_results)?,
                next_prompts.join("\n")
            );
            self.state
                .messages
                .push(ChatMessage::user(next_user_prompt.clone()));
            last_response = Some(response);
        }

        Ok(RuntimeOutcome {
            result: "MAX_TURNS_EXCEEDED".to_string(),
            final_response: last_response,
            exit_data: Some(json!({ "last_prompt": next_user_prompt })),
        })
    }

    fn trim_context(&mut self) {
        for message in &mut self.state.messages {
            message.content = trim_history_tags(&message.content, 800);
        }
        let mut cost: usize = self.state.messages.iter().map(|m| m.content.len()).sum();
        while self.state.messages.len() > 9 && cost > self.options.context_char_budget {
            self.state.messages.remove(0);
            while self
                .state
                .messages
                .first()
                .is_some_and(|m| m.role != crate::Role::User)
            {
                self.state.messages.remove(0);
            }
            cost = self.state.messages.iter().map(|m| m.content.len()).sum();
        }
    }

    fn make_tool_context(&self, tool_index: usize, tool_count: usize) -> ToolContext {
        ToolContext {
            gars_home: self.options.gars_home.clone(),
            cwd: self.options.cwd.clone(),
            current_turn: self.state.current_turn,
            tool_index,
            tool_count,
            working: self.state.working.clone(),
            history_info: self.state.history_info.clone(),
        }
    }

    async fn dispatch_tool(
        &self,
        call: &ToolCall,
        ctx: &mut ToolContext,
    ) -> Result<crate::StepOutcome> {
        if call.name == "bad_json" {
            return Ok(crate::StepOutcome {
                data: Some(json!({ "status": "error", "msg": call.arguments })),
                next_prompt: Some("bad_json: regenerate a valid tool_use block".to_string()),
                should_exit: false,
            });
        }
        self.tools
            .execute(&call.name, call.arguments.clone(), ctx)
            .await
    }

    fn record_summary(&mut self, response: &ChatResponse) {
        let summary = extract_summary(&response.content).unwrap_or_else(|| {
            if response.tool_calls.is_empty() {
                "直接回答了用户问题".to_string()
            } else {
                format!("调用工具{}", response.tool_calls[0].name)
            }
        });
        self.state.history_info.push(format!(
            "[Agent] {}",
            smart_truncate(summary.replace('\n', ""), 80)
        ));
    }
}

fn extract_summary(content: &str) -> Option<String> {
    let start = content.find("<summary>")? + "<summary>".len();
    let end = content[start..].find("</summary>")? + start;
    Some(content[start..end].trim().to_string())
}

#[cfg(test)]
mod tests {
    use anyhow::Result;
    use async_trait::async_trait;
    use serde_json::json;

    use crate::{ChatRequest, ChatResponse, LlmClient, ToolCall, ToolRegistry};

    use super::*;

    struct MockClient {
        calls: usize,
    }

    #[async_trait]
    impl LlmClient for MockClient {
        fn name(&self) -> &str {
            "mock"
        }

        async fn chat(&mut self, _request: ChatRequest) -> Result<ChatResponse> {
            self.calls += 1;
            if self.calls == 1 {
                Ok(ChatResponse {
                    content: "<summary>read file</summary>".to_string(),
                    tool_calls: vec![ToolCall::new("bad_json", json!({"oops": true}))],
                    ..Default::default()
                })
            } else {
                Ok(ChatResponse {
                    content: "done".to_string(),
                    ..Default::default()
                })
            }
        }
    }

    #[tokio::test]
    async fn runtime_recovers_from_bad_json() {
        let mut rt = AgentRuntime::new(
            MockClient { calls: 0 },
            ToolRegistry::new(),
            "",
            RuntimeOptions::default(),
        );
        let out = rt.run_once("hi", |_| {}).await.unwrap();
        assert_eq!(out.result, "CURRENT_TASK_DONE");
    }
}
