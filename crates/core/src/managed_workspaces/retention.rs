//! 到期只表示可以检查，不表示可以删。复用 finish 的锁、资源核对和产物验证。

use super::*;
use crate::tools::Caller;
use serde_json::{json, Value};

impl Operation {
    pub fn set_retention(&mut self, hours: Option<u64>) -> Result<()> {
        self.require_active()?;
        if hours.is_some_and(|hours| !(1..=8760).contains(&hours)) {
            return Err(WorkspaceError::invalid_argument(
                "retention_hours must be 1..=8760; retain_forever=true pins the directory",
            ));
        }
        self.record.idle_retention_ms = hours.map(|hours| hours * 60 * 60 * 1000);
        self.record.last_used_at_ms = now_ms();
        self.record.expires_at_ms = self
            .record
            .idle_retention_ms
            .map(|ttl| self.record.last_used_at_ms.saturating_add(ttl));
        self.persist()
    }
}

impl Store {
    pub fn sweep(
        &self,
        owner: &str,
        cursor: Option<&str>,
        limit: usize,
        caller: Option<&Caller>,
    ) -> Result<Value> {
        self.sweep_at(owner, cursor, limit, caller, now_ms())
    }

    fn sweep_at(
        &self,
        owner: &str,
        cursor: Option<&str>,
        limit: usize,
        caller: Option<&Caller>,
        now: u64,
    ) -> Result<Value> {
        if !(1..=16).contains(&limit) || cursor.is_some_and(|id| !is_id(id)) {
            return Err(WorkspaceError::invalid_argument(
                "sweep takes limit 1..=16 and an optional returned cursor",
            ));
        }
        let records = self.live()?;
        let selected: Vec<_> = records
            .iter()
            .filter(|r| {
                r.owner == owner
                    && r.state != State::Closed
                    && r.expires_at_ms.is_some_and(|at| at <= now)
                    && cursor.is_none_or(|id| r.id.as_str() > id)
            })
            .take(limit + 1)
            .collect();
        let more = selected.len() > limit;
        let selected = &selected[..selected.len().min(limit)];
        let mut removed = Vec::new();
        let mut blocked = Vec::new();
        for record in selected {
            let result: Result<bool> = (|| {
                let mut op = self.operation(owner, &record.id)?;
                // 拿锁之后再查；前面的名单只是快照，用户可能刚继续使用或要求保留。
                if op.record.state == State::Closed
                    || op.record.expires_at_ms.is_none_or(|at| at > now)
                {
                    return Ok(false);
                }
                let paths = op.record.finish_paths.clone().unwrap_or_default();
                op.finish(&paths, caller)?;
                Ok(true)
            })();
            match result {
                Ok(true) => removed.push(record.id.clone()),
                Ok(false) => {}
                Err(e) => blocked
                    .push(json!({"workspace":record.id,"code":e.code(),"reason":e.message()})),
            }
        }
        Ok(
            json!({"removed":removed,"blocked":blocked,"examined":selected.len(),
            "next_cursor":if more {selected.last().map(|r| &r.id)} else {None},
            "note":"Only expired directories of this caller were checked. Artifacts and close receipts are retained; unknown or busy resources were not deleted."}),
        )
    }
}

#[cfg(test)]
mod tests;
