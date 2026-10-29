//! The wonders as the Behaviors view sends them to the screen.

use argonauts_core::app::{AppEvent, UiCommand, ViewData, ViewRequest};
use argonauts_core::behavior_map::{MapRequest, Scope};
use argonauts_core::behavior_world::Wonder;
use argonauts_core::events::run::RunOutcome;

use super::three_makers;
use crate::controller::test_support::TestDriver;

async fn wonders_shown(driver: &mut TestDriver, scope: Scope) -> Vec<Wonder> {
    driver
        .send(UiCommand::Request {
            view: ViewRequest::Behaviors(MapRequest {
                scope,
                ..MapRequest::default()
            }),
        })
        .await;
    let shown = driver
        .until(|event| {
            matches!(event, AppEvent::View { data } if matches!(**data, ViewData::Behaviors { .. }))
        })
        .await;
    let AppEvent::View { data } = shown else {
        unreachable!()
    };
    let ViewData::Behaviors { map } = *data else {
        unreachable!()
    };
    let world = map.world.expect("the map carries its world");
    world.wonders.iter().map(|risen| risen.wonder).collect()
}

#[tokio::test]
async fn the_map_shows_the_crews_wonders_whatever_project_it_covers() {
    let mut driver = TestDriver::start().await;
    // The run is another project's, so this project's map holds none of its actions.
    three_makers("run-argo", RunOutcome::Merged).write(driver.repo.home());
    for scope in [Scope::Everywhere, Scope::ThisProject] {
        assert_eq!(wonders_shown(&mut driver, scope).await, [Wonder::Argo]);
    }
    assert!(driver.agent.specs().is_empty(), "no session was started");
}
