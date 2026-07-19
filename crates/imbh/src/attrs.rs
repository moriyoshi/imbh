//! Attribute discovery (ARCHITECTURE.md §10.9) — the Loki `labels` / `label/{n}/values` shape.
//!
//! `db.attrs().names()` enumerates distinct attribute keys; `db.attrs().values(key)` enumerates
//! distinct values. Discovery is **cross-signal**: it sweeps every table ([`Table::ALL`] — logs,
//! spans, and the five metric families), unioning the `attributes` column (parsed with the shared
//! JSON parser) plus the promoted `service` column, so `names()` answers "what labels exist across
//! my telemetry". A host that wants per-signal scoping (e.g. a Loki-compatible logs-only endpoint)
//! can issue the equivalent `SELECT DISTINCT … FROM logs` through [`Db::sql`] directly.
//!
//! The plan's near-free path — reading Tantivy term dictionaries and Arrow dictionary pages with no
//! data scan (§10) — is a later optimization; results are identical.

use std::collections::BTreeSet;

use imbh_core::{Attributes, Table};

use std::sync::Arc;

use crate::logs::get_str;
use crate::sql::SqlParams;
use crate::{Db, Result};

/// Attribute discovery namespace, reached via [`Db::attrs`].
pub struct AttrsApi {
    pub(crate) db: Arc<Db>,
}

/// Build a `UNION` across every signal table from a per-table `SELECT` fragment. `UNION` (not
/// `UNION ALL`) dedups across tables; each fragment already selects `DISTINCT`, so per-table dedup
/// happens before the union.
fn across_all_tables(fragment: impl Fn(&str) -> String) -> String {
    Table::ALL
        .iter()
        .map(|t| fragment(t.as_str()))
        .collect::<Vec<_>>()
        .join(" UNION ")
}

impl AttrsApi {
    /// Distinct attribute keys present on any signal (logs, spans, metrics), plus `service.name`
    /// when any record carries a service. Sorted.
    pub async fn names(&self) -> Result<Vec<String>> {
        let mut keys: BTreeSet<String> = BTreeSet::new();

        // The promoted service column surfaces as the OTel `service.name` key.
        let svc = self
            .db
            .sql(&across_all_tables(|t| {
                format!("SELECT DISTINCT service FROM {t} WHERE service IS NOT NULL")
            }))
            .collect()
            .await?;
        if svc.iter().any(|b| b.num_rows() > 0) {
            keys.insert("service.name".to_owned());
        }

        // Distinct attribute blobs → union of their keys (DISTINCT keeps the parse cost down).
        let batches = self
            .db
            .sql(&across_all_tables(|t| {
                format!("SELECT DISTINCT attributes FROM {t}")
            }))
            .collect()
            .await?;
        for b in &batches {
            let col = b.column(0);
            for i in 0..b.num_rows() {
                if let Some(s) = get_str(col.as_ref(), i) {
                    for (k, _) in Attributes::from_canonical_json(&s).iter() {
                        keys.insert(k.to_owned());
                    }
                }
            }
        }
        Ok(keys.into_iter().collect())
    }

    /// Distinct string values for `key` across every signal (sorted). `service.name` resolves to the
    /// promoted column; any other key resolves through `json_get_str` over the attributes column.
    pub async fn values(&self, key: &str) -> Result<Vec<String>> {
        let mut params = SqlParams::with_promote(self.db.storage.promote().keys());
        let sql = if key == "service.name" {
            across_all_tables(|t| {
                format!("SELECT DISTINCT service AS v FROM {t} WHERE service IS NOT NULL")
            })
        } else {
            // The field expression (promoted dict column when `key` is promoted, else a
            // `json_get_str` scan) is reused at both spots in every per-table fragment.
            let f = params.attr_field(key);
            across_all_tables(|t| {
                format!("SELECT DISTINCT {f} AS v FROM {t} WHERE {f} IS NOT NULL")
            })
        };
        let batches = self
            .db
            .sql_with_params(sql, params.into_values())
            .collect()
            .await?;
        let mut values: BTreeSet<String> = BTreeSet::new();
        for b in &batches {
            let col = b.column(0);
            for i in 0..b.num_rows() {
                if let Some(s) = get_str(col.as_ref(), i) {
                    values.insert(s);
                }
            }
        }
        Ok(values.into_iter().collect())
    }
}
