//! Jobs that UI pages hand to a Web Worker (`web/worker.js`), so that searching many tabs,
//! reading a large import and fixing up a recording don't hold up drawing and typing.
//!
//! The worker loads this same wasm and calls `worker_run(op, text, bytes)` for each job.
//! Pages fall back to running the same functions themselves while the worker starts or
//! if it can't run (see `ui/offload.rs`), so a job must give the same answer either way.
//!
//! | op                | text in                          | out                                          |
//! |-------------------|----------------------------------|----------------------------------------------|
//! | `"state"`         | the [`AppState`] (JSON)          | `""`; kept for the searches that follow       |
//! | `"search"`        | a [`SearchRequest`] (JSON)       | `Result<Vec<Id>, String>` (JSON)              |
//! | `"import"`        | the text to import               | `Result<Imported, String>` (JSON)             |
//! | `"webm-duration"` | the duration in ms; bytes: head  | the new head (bytes), or the error (text)     |
//!
//! An `Err` from [`Worker::run`] means the job didn't run (the page then runs it itself),
//! not that its answer is an error.

use crate::model::{AppState, Id};
use crate::search::{self, SearchRequest};
use crate::{transfer, webm};

#[derive(Debug, PartialEq)]
pub enum Output {
    Text(String),
    Bytes(Vec<u8>),
}

#[derive(Default)]
pub struct Worker {
    /// The state searches run on (sent by the page when its revision changes).
    state: Option<AppState>,
}

impl Worker {
    pub fn run(&mut self, op: &str, text: &str, bytes: Vec<u8>) -> Result<Output, String> {
        match op {
            "state" => {
                self.state = Some(serde_json::from_str(text).map_err(|e| format!("state: {e}"))?);
                Ok(Output::Text(String::new()))
            }
            "search" => {
                let req: SearchRequest = serde_json::from_str(text).map_err(|e| format!("search: {e}"))?;
                // A page that thinks the worker has its state when it doesn't (the worker was
                // stopped and started again) searches by itself and sends the state again.
                let state = self.state.as_ref().filter(|s| s.revision == req.revision).ok_or("search: not the page's state")?;
                let ids: Result<Vec<Id>, String> = search::matching_ids(state, &req).map(|ids| ids.into_iter().collect());
                Ok(Output::Text(to_json(&ids)))
            }
            "import" => Ok(Output::Text(to_json(&transfer::parse_import(text)))),
            "webm-duration" => {
                let ms: f64 = text.parse().map_err(|_| format!("webm-duration: bad duration {text:?}"))?;
                Ok(match webm::set_duration(&bytes, ms) {
                    Ok(head) => Output::Bytes(head),
                    Err(e) => Output::Text(e),
                })
            }
            other => Err(format!("unknown worker job {other:?}")),
        }
    }
}

fn to_json<T: serde::Serialize>(v: &T) -> String {
    serde_json::to_string(v).unwrap_or_else(|e| serde_json::to_string(&Err::<(), _>(e.to_string())).unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Session, TabEntry};
    use crate::transfer::Imported;

    fn state() -> AppState {
        let mut s = AppState { revision: 7, ..Default::default() };
        let entries = [(10, "https://docs.rs/egui", "egui docs"), (11, "https://example.com/", "Example")]
            .map(|(id, url, title)| TabEntry { id, url: url.into(), title: title.into(), ..Default::default() });
        s.sessions.push(Session { id: 2, name: "Work".into(), entries: entries.to_vec(), ..Default::default() });
        s
    }

    fn text(out: Output) -> String {
        match out {
            Output::Text(t) => t,
            Output::Bytes(_) => panic!("expected text"),
        }
    }

    #[test]
    fn search_runs_on_the_state_sent() {
        let mut w = Worker::default();
        let req = SearchRequest { query: "egui".into(), revision: 7, ..Default::default() };
        let req_json = serde_json::to_string(&req).unwrap();
        assert!(w.run("search", &req_json, vec![]).is_err(), "no state yet");

        w.run("state", &serde_json::to_string(&state()).unwrap(), vec![]).unwrap();
        let ids: Result<Vec<Id>, String> = serde_json::from_str(&text(w.run("search", &req_json, vec![]).unwrap())).unwrap();
        assert_eq!(ids, Ok(vec![10]));
        assert_eq!(search::matching_ids(&state(), &req).unwrap().into_iter().collect::<Vec<_>>(), vec![10], "same answer on the page");

        let stale = SearchRequest { revision: 6, ..req.clone() };
        assert!(w.run("search", &serde_json::to_string(&stale).unwrap(), vec![]).is_err(), "another revision");

        let bad = SearchRequest { query: "is:nonsense".into(), ..req };
        let ids: Result<Vec<Id>, String> =
            serde_json::from_str(&text(w.run("search", &serde_json::to_string(&bad).unwrap(), vec![]).unwrap())).unwrap();
        assert!(ids.is_err(), "a bad query is an answer, not a failed job");
    }

    #[test]
    fn import_round_trips() {
        let mut w = Worker::default();
        let out = text(w.run("import", "https://a.example/, https://b.example/", vec![]).unwrap());
        let parsed: Result<Imported, String> = serde_json::from_str(&out).unwrap();
        assert_eq!(parsed, transfer::parse_import("https://a.example/, https://b.example/"));
        let out = text(w.run("import", "url,name\nhttps://a.example/,A\n", vec![]).unwrap());
        assert!(matches!(serde_json::from_str::<Result<Imported, String>>(&out).unwrap(), Ok(Imported::Table(_))));
    }

    #[test]
    fn webm_errors_come_back_as_text() {
        let mut w = Worker::default();
        assert!(matches!(w.run("webm-duration", "1000", vec![1, 2, 3]).unwrap(), Output::Text(_)));
        assert!(w.run("webm-duration", "soon", vec![]).is_err());
        assert!(w.run("nope", "", vec![]).is_err());
    }
}
