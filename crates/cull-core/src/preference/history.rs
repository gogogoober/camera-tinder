//! Every proposed model: what it replaced and what the user decided (active, retired or declined; the timeline is in
//! `preference_events`). Weights are never edited; reverting re-activates an older row, so any change can be undone.

use rusqlite::{OptionalExtension, Row, params};
use serde::{Deserialize, Serialize};

use super::Weights;
use super::train::Proposal;
use crate::analysis::{AnalysisKind, Analyzers};
use crate::error::{CoreError, Result};
use crate::model::{Decision, PhotoId};
use crate::store::{Store, TIME_FMT, now};

pub const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS preference_models (
    id         INTEGER PRIMARY KEY,
    created_at TEXT NOT NULL,
    parent_id  INTEGER REFERENCES preference_models(id), -- the active model it was compared with
    status     TEXT NOT NULL,                            -- active | retired | declined
    weights    TEXT NOT NULL,
    metrics    TEXT NOT NULL,
    changes    TEXT NOT NULL,                            -- weight moves against the parent
    note       TEXT
);
CREATE TABLE IF NOT EXISTS preference_events (
    id          INTEGER PRIMARY KEY,
    at          TEXT NOT NULL,
    action      TEXT NOT NULL,                           -- approve | decline | revert
    model_id    INTEGER NOT NULL REFERENCES preference_models(id),
    previous_id INTEGER                                  -- the active model before this event
);
"#;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredModel {
    pub id: i64,
    pub created_at: String,
    pub parent_id: Option<i64>,
    pub status: String,
    pub weights: Weights,
    pub metrics: serde_json::Value,
    pub changes: serde_json::Value,
    pub note: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Approve,
    Decline,
}

fn other(e: serde_json::Error) -> CoreError {
    CoreError::Other(e.to_string())
}

fn to_model(row: &Row) -> rusqlite::Result<(StoredModel, String, String, String)> {
    let model = StoredModel {
        id: row.get(0)?,
        created_at: row.get(1)?,
        parent_id: row.get(2)?,
        status: row.get(3)?,
        weights: Weights { bias: 0.0, features: Vec::new() },
        metrics: serde_json::Value::Null,
        changes: serde_json::Value::Null,
        note: row.get(7)?,
    };
    Ok((model, row.get(4)?, row.get(5)?, row.get(6)?))
}

const COLS: &str = "id, created_at, parent_id, status, weights, metrics, changes, note";

fn finish((mut m, w, me, ch): (StoredModel, String, String, String)) -> Result<StoredModel> {
    m.weights = serde_json::from_str(&w).map_err(other)?;
    m.metrics = serde_json::from_str(&me).map_err(other)?;
    m.changes = serde_json::from_str(&ch).map_err(other)?;
    Ok(m)
}

impl Store {
    /// The approved model the app ranks with, if any.
    pub fn active_preference(&self) -> Result<Option<StoredModel>> {
        let row = self.conn.query_row(&format!("SELECT {COLS} FROM preference_models WHERE status = 'active'"), [], to_model).optional()?;
        row.map(finish).transpose()
    }

    /// Every proposal ever made, newest first.
    pub fn preference_models(&self) -> Result<Vec<StoredModel>> {
        let mut stmt = self.conn.prepare(&format!("SELECT {COLS} FROM preference_models ORDER BY id DESC"))?;
        let rows = stmt.query_map([], to_model)?.collect::<std::result::Result<Vec<_>, _>>()?;
        rows.into_iter().map(finish).collect()
    }

    /// Record the user's answer to a proposal. Approving makes it the active model and retires the one before.
    /// Returns the new model's id.
    pub fn record_preference(&self, proposal: &Proposal, verdict: Verdict, note: Option<&str>) -> Result<i64> {
        let tx = self.conn.unchecked_transaction()?;
        let previous: Option<i64> = tx.query_row("SELECT id FROM preference_models WHERE status = 'active'", [], |r| r.get(0)).optional()?;
        let status = if verdict == Verdict::Approve { "active" } else { "declined" };
        if verdict == Verdict::Approve {
            tx.execute("UPDATE preference_models SET status = 'retired' WHERE status = 'active'", [])?;
        }
        let at = now();
        tx.execute(
            "INSERT INTO preference_models (created_at, parent_id, status, weights, metrics, changes, note) VALUES (?, ?, ?, ?, ?, ?, ?)",
            params![
                at,
                previous,
                status,
                serde_json::to_string(&proposal.weights).map_err(other)?,
                serde_json::to_string(&proposal.metrics).map_err(other)?,
                serde_json::to_string(&proposal.changes).map_err(other)?,
                note
            ],
        )?;
        let id = tx.last_insert_rowid();
        let action = if verdict == Verdict::Approve { "approve" } else { "decline" };
        tx.execute("INSERT INTO preference_events (at, action, model_id, previous_id) VALUES (?, ?, ?, ?)", params![at, action, id, previous])?;
        tx.commit()?;
        Ok(id)
    }

    /// Go back to an older model: it becomes the active one again, the current one is retired.
    pub fn revert_preference(&self, id: i64) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        let exists: Option<i64> = tx.query_row("SELECT id FROM preference_models WHERE id = ?", [id], |r| r.get(0)).optional()?;
        if exists.is_none() {
            return Err(CoreError::Other(format!("no model with id {id}")));
        }
        let previous: Option<i64> = tx.query_row("SELECT id FROM preference_models WHERE status = 'active'", [], |r| r.get(0)).optional()?;
        if previous == Some(id) {
            return Ok(());
        }
        tx.execute("UPDATE preference_models SET status = 'retired' WHERE status = 'active'", [])?;
        tx.execute("UPDATE preference_models SET status = 'active' WHERE id = ?", [id])?;
        let at = now();
        tx.execute("INSERT INTO preference_events (at, action, model_id, previous_id) VALUES (?, 'revert', ?, ?)", params![at, id, previous])?;
        tx.commit()?;
        Ok(())
    }

    /// Stop using any learned model: the app goes back to the hand-tuned ranking. History stays, and the event is recorded.
    pub fn deactivate_preference(&self) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        let Some(active): Option<i64> = tx.query_row("SELECT id FROM preference_models WHERE status = 'active'", [], |r| r.get(0)).optional()? else {
            return Ok(());
        };
        tx.execute("UPDATE preference_models SET status = 'retired' WHERE id = ?", [active])?;
        tx.execute("INSERT INTO preference_events (at, action, model_id, previous_id) VALUES (?, 'off', ?, ?)", params![now(), active, active])?;
        tx.commit()?;
        Ok(())
    }

    /// Every decided photo with its analysis from the engines now in use (older results are left out).
    pub fn preference_samples(&self, analyzers: &Analyzers) -> Result<Vec<super::train::Sample>> {
        let mut stmt = self.conn.prepare("SELECT d.photo_id, d.decision, d.decided_at, p.captured_at FROM decisions d JOIN photos p ON p.id = d.photo_id")?;
        let rows: Vec<(PhotoId, String, String, Option<String>)> =
            stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?.collect::<std::result::Result<_, _>>()?;
        let ids: Vec<PhotoId> = rows.iter().map(|r| r.0).collect();
        let mut analyses = self.load_analyses(&ids, None)?;
        let mut out = Vec::new();
        for (id, decision, decided_at, captured) in rows {
            let Some(decision) = Decision::parse(&decision) else { continue };
            let Some(mut analysis) = analyses.remove(&id) else { continue };
            analysis.retain_current(analyzers);
            if !AnalysisKind::ALL.into_iter().any(|k| analysis.has(k)) {
                continue;
            }
            out.push(super::train::Sample {
                photo_id: id,
                captured_at: captured.and_then(|s| chrono::NaiveDateTime::parse_from_str(&s, TIME_FMT).ok()),
                keep: decision.is_keeper(),
                favorite: decision == Decision::Favorite,
                decided_at,
                analysis,
            });
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::preference::train::{Metrics, Eval};
    use crate::preference::{FeatureWeight, weight_changes};

    fn proposal(weight: f32) -> Proposal {
        let weights = Weights { bias: 0.0, features: vec![FeatureWeight { name: "aesthetics".into(), weight, mean: 0.0, std: 1.0 }] };
        let e = Eval { auc: Some(0.6), burst_hits: 1, burst_groups: 2, burst_chance: 1.0 };
        Proposal {
            changes: weight_changes(None, &weights),
            weights,
            metrics: Metrics { decisions: 10, keeps: 5, nopes: 5, favorites: 0, trained_on: 8, held_out: 2, pair_groups: 0, people_decisions: 0, people_keeps: 0, current_tested_on: 0, candidate: e, current: e, hand_tuned: e },
            separation: Vec::new(),
        }
    }

    #[test]
    fn approve_decline_and_revert_keep_a_full_record() {
        let store = Store::open_in_memory().unwrap();
        assert!(store.active_preference().unwrap().is_none());

        let first = store.record_preference(&proposal(1.0), Verdict::Approve, None).unwrap();
        let declined = store.record_preference(&proposal(9.0), Verdict::Decline, Some("too big a jump")).unwrap();
        assert_eq!(store.active_preference().unwrap().unwrap().id, first, "a declined proposal never becomes active");
        let second = store.record_preference(&proposal(2.0), Verdict::Approve, None).unwrap();

        let active = store.active_preference().unwrap().unwrap();
        assert_eq!((active.id, active.parent_id), (second, Some(first)));
        assert_eq!(active.weights.weight_of("aesthetics"), Some(2.0));

        store.revert_preference(first).unwrap();
        assert_eq!(store.active_preference().unwrap().unwrap().weights.weight_of("aesthetics"), Some(1.0));

        let all = store.preference_models().unwrap();
        let status: Vec<(&i64, &str)> = all.iter().map(|m| (&m.id, m.status.as_str())).collect();
        assert_eq!(status, vec![(&second, "retired"), (&declined, "declined"), (&first, "active")]);
        let events: i64 = store.conn.query_row("SELECT COUNT(*) FROM preference_events", [], |r| r.get(0)).unwrap();
        assert_eq!(events, 4, "approve, decline, approve, revert");
        assert!(store.revert_preference(999).is_err());

        store.deactivate_preference().unwrap();
        assert!(store.active_preference().unwrap().is_none());
        let off: i64 = store.conn.query_row("SELECT COUNT(*) FROM preference_events WHERE action = 'off' AND model_id = ?", [first], |r| r.get(0)).unwrap();
        assert_eq!(off, 1, "turning the model off is recorded");
        store.deactivate_preference().unwrap();
        let events: i64 = store.conn.query_row("SELECT COUNT(*) FROM preference_events", [], |r| r.get(0)).unwrap();
        assert_eq!(events, 5, "turning off when nothing is active records nothing");
    }
}
