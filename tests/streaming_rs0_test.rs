use agentgate::provider::CliProviderImpl;
use agentgate::provider::CliProvider;
use agentgate::reasoning::RunEvent;
use agentgate::types::ChatCompletionRequest;
use std::time::Instant;
use tokio::sync::mpsc;

#[tokio::test]
async fn rs0_first_reasoning_arrives_before_completion() {
    let bin = env!("CARGO_BIN_EXE_fake_cli_thinking");
    let provider = CliProviderImpl {
        id: "fake".to_string(),
        bin: Some(bin.to_string()),
        models_cmd: None,
        chat_cmd: Some(vec![]),
        strip_ansi: false,
        models: vec![],
        is_available: true,
    };

    let (tx, mut rx) = mpsc::channel(128);
    let start = Instant::now();
    let handle = tokio::spawn(async move {
        provider
            .send_chat_completion_stream("m", ChatCompletionRequest {
                model: "m".to_string(),
                messages: vec![],
                ..Default::default()
            }, tx)
            .await
    });

    let mut first_reasoning = None;
    while let Some(ev) = rx.recv().await {
        if matches!(ev, RunEvent::ReasoningDelta { .. }) && first_reasoning.is_none() {
            first_reasoning = Some(start.elapsed());
        }
        if matches!(ev, RunEvent::Done { .. } | RunEvent::Error { .. }) {
            break;
        }
    }
    let total = start.elapsed();
    let fr = first_reasoning.expect("reasoning delta received");
    assert!(fr < total, "first reasoning {:?} not before total {:?}", fr, total);
    handle.await.unwrap().unwrap();
}
