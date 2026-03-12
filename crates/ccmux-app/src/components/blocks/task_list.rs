use dioxus::prelude::*;

use ccmux_core::display::{TaskItem, TaskStatus};

#[component]
pub fn TaskListBlock(tasks: Vec<TaskItem>) -> Element {
    rsx! {
        div { class: "task-list-block",
            for task in &tasks {
                div { class: "task-item",
                    span { class: "task-checkbox {task_status_class(&task.status)}",
                        match task.status {
                            TaskStatus::Completed => rsx! { "[x]" },
                            TaskStatus::InProgress => rsx! { "[~]" },
                            TaskStatus::Cancelled => rsx! { "[-]" },
                            TaskStatus::Pending => rsx! { "[ ]" },
                        }
                    }
                    span { class: "task-subject", "{task.subject}" }
                }
            }
        }
    }
}

fn task_status_class(status: &TaskStatus) -> &'static str {
    match status {
        TaskStatus::Pending => "task-pending",
        TaskStatus::InProgress => "task-in-progress",
        TaskStatus::Completed => "task-completed",
        TaskStatus::Cancelled => "task-cancelled",
    }
}
