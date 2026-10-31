pub(crate) mod golden;
pub(crate) mod story;

use super::*;

#[test]
fn each_setting_picks_its_look() {
    use Bubbles::*;
    use CouncilView::*;
    assert_eq!(Look::choose(Messages, Auto, true), Look::Pictures);
    assert_eq!(Look::choose(Messages, Auto, false), Look::Transcript);
    assert_eq!(Look::choose(Messages, Blocks, false), Look::Blocks);
    assert_eq!(Look::choose(Messages, Plain, true), Look::Plain);
    assert_eq!(Look::choose(Transcript, Auto, true), Look::Transcript);
}
