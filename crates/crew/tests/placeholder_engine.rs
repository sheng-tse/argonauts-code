use argonauts_core::engine::EngineExit;
use argonauts_core::events::RunEventKind;
use argonauts_core::events::run::{RunLifecycle, RunMode, RunOutcome};
use argonauts_core::settings::Settings;
use argonauts_testkit::services::{FakeServices, engine_context};

#[tokio::test]
async fn the_placeholder_engine_records_a_failed_finish() {
    let services = FakeServices::new();
    let (context, sink, _commands) = engine_context(&services, Settings::default());
    let exit = argonauts_crew::engine_for(&Settings::default(), RunMode::Solo)
        .run(context)
        .await;
    assert_eq!(exit, EngineExit::Finished(RunOutcome::Failed));
    let events = sink.events();
    assert_eq!(events.len(), 1);
    match &events[0].kind {
        RunEventKind::Run(RunLifecycle::Finished { outcome, detail }) => {
            assert_eq!(*outcome, RunOutcome::Failed);
            assert!(
                detail
                    .as_deref()
                    .unwrap()
                    .contains(argonauts_crew::ENGINE_NOT_BUILT)
            );
        }
        other => panic!("{other:?}"),
    }
}
