use std::cmp::Reverse;
use std::mem::take;

use itertools::Itertools as _;

use crate::utils::format_to;

#[allow(clippy::module_name_repetitions)] // Re-exported in the super module.
pub struct StatusChangeDetector {
    state: State,
    errors: Vec<String>,
}

enum State {
    Initial,
    Ok,
    Fail,
}

/// An indication on whether the service went up or down.
#[derive(Debug, Eq, PartialEq)]
enum StatusChange {
    Up,
    Down(String),
}

impl StatusChangeDetector {
    pub const fn new() -> Self {
        Self { state: State::Initial, errors: Vec::new() }
    }

    pub fn log_on_change(
        &mut self,
        name: &str,
        input: Result<(), anyhow::Error>,
    ) {
        match self.tick(input) {
            Some(StatusChange::Up) => log::info!("{name} went up"),
            Some(StatusChange::Down(e)) => log::warn!("{name} went down: {e}"),
            None => (),
        }
    }

    fn tick(
        &mut self,
        input: Result<(), anyhow::Error>,
    ) -> Option<StatusChange> {
        match input {
            Ok(()) => {
                self.errors.clear();
                let result = match self.state {
                    State::Initial | State::Fail => Some(StatusChange::Up),
                    State::Ok => None,
                };
                self.state = State::Ok;
                result
            }
            Err(e) => match self.state {
                State::Initial => {
                    self.state = State::Fail;
                    self.errors.push(e.to_string());
                    debug_assert_eq!(self.errors.len(), 1);
                    Some(StatusChange::Down(e.to_string()))
                }
                State::Ok => {
                    self.errors.push(e.to_string());
                    (self.errors.len() >= 10).then(|| {
                        self.state = State::Fail;
                        StatusChange::Down(join_errors(take(&mut self.errors)))
                    })
                }
                State::Fail => None,
            },
        }
    }
}

fn join_errors(mut errors: Vec<String>) -> String {
    errors.sort();
    let mut result = String::new();
    for (count, error) in errors
        .iter()
        .dedup_with_count()
        .sorted_by_key(|(count, _)| Reverse(*count))
    {
        if count > 1 {
            format_to!(result, "{count}× {error}\n");
        } else {
            format_to!(result, "{error}\n");
        }
    }
    result.pop(); // Remove the trailing newline
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test() {
        let mut ed = StatusChangeDetector::new();
        run_checks(&mut ed);

        let mut ed = StatusChangeDetector::new();
        // The initial error should be reported.
        assert_eq!(
            ed.tick(Err(anyhow::anyhow!("err"))),
            Some(StatusChange::Down("err".to_string()))
        );
        // The subsequent errors should not trigger a state change.
        for _ in 0..10 {
            assert_eq!(ed.tick(Err(anyhow::anyhow!("err"))), None);
        }
        run_checks(&mut ed);
    }

    fn run_checks(ed: &mut StatusChangeDetector) {
        // Assume that state is initial or down. A success should be reported.
        assert_eq!(ed.tick(Ok(())), Some(StatusChange::Up));

        // Subsequent successes should not trigger a state change.
        assert_eq!(ed.tick(Ok(())), None);

        // 9 errors with a single success should not trigger a state change.
        for _ in 0..2 {
            for _ in 0..9 {
                assert_eq!(ed.tick(Err(anyhow::anyhow!("err"))), None);
            }
            assert_eq!(ed.tick(Ok(())), None);
        }

        // Assume the state is now up.
        // For the first 9 errors, we should not get any updates.
        for _ in 0..6 {
            assert_eq!(ed.tick(Err(anyhow::anyhow!("err1"))), None);
        }
        for _ in 6..9 {
            assert_eq!(ed.tick(Err(anyhow::anyhow!("err2"))), None);
        }

        // For the 10th error, we should get an indication that we're going
        // down.
        assert_eq!(
            ed.tick(Err(anyhow::anyhow!("err3"))),
            Some(StatusChange::Down("6× err1\n3× err2\nerr3".to_string()))
        );

        // Since state is now Fail, we should not get any more updates.
        for _ in 0..10 {
            assert_eq!(ed.tick(Err(anyhow::anyhow!("err"))), None);
        }
    }
}
