//! A task's to-do count: how much of the list its own session keeps is done. The run list counts
//! a task done in its own session by it.

use argonauts_core::{TodoItem, TodoStatus};

/// Done of total in a session's to-do list, or `None` for an empty one, which counts nothing.
pub(crate) fn count(items: &[TodoItem]) -> Option<(u32, u32)> {
    if items.is_empty() {
        return None;
    }
    let done = items
        .iter()
        .filter(|item| item.status == TodoStatus::Completed)
        .count();
    let counted = |n: usize| u32::try_from(n).unwrap_or(u32::MAX);
    Some((counted(done), counted(items.len())))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(text: &str, status: TodoStatus) -> TodoItem {
        TodoItem {
            text: text.into(),
            status,
        }
    }

    #[test]
    fn a_list_counts_what_is_done_of_all_of_it() {
        let items = [
            item("Read the export code", TodoStatus::Completed),
            item("Add the flag", TodoStatus::InProgress),
            item("Write the tests", TodoStatus::Pending),
        ];
        assert_eq!(count(&items), Some((1, 3)));
        assert_eq!(count(&[]), None);
    }
}
