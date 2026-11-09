//! File copies run outside the actor; publication still belongs to its original turn. Artifact cards post the same way.
use super::Actor;
use crate::chat::files::{self, Snapshot};
use crate::store::EntryKind;
use anyhow::{Result, anyhow, ensure};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use tokio::sync::{mpsc, oneshot};

type Reply = oneshot::Sender<Result<()>>;
pub(super) struct Completed {
    epoch: u64,
    cancelled: Arc<AtomicBool>,
    lane: crate::store::Lane,
    delivery_id: Option<String>,
    reply: Reply,
    result: Result<Snapshot>,
}

pub(super) struct FileShares {
    epoch: u64,
    pub summary: Option<String>,
    pending: Vec<Arc<AtomicBool>>,
    pub tx: mpsc::UnboundedSender<Completed>,
    pub rx: mpsc::UnboundedReceiver<Completed>,
}
impl Default for FileShares {
    fn default() -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        Self { epoch: 0, summary: None, pending: Vec::new(), tx, rx }
    }
}
impl FileShares {
    pub fn cancel(&mut self) {
        self.epoch = self.epoch.wrapping_add(1);
        for flag in self.pending.drain(..) {
            flag.store(true, Ordering::Release);
        }
    }
}
impl Drop for FileShares {
    fn drop(&mut self) {
        self.cancel();
    }
}

impl Actor {
    /// 复制前冻结聊天与投递身份，完成时不能把旧文件发布到下一任务。
    pub(super) fn begin_file(
        &mut self,
        source: String,
        name: Option<String>,
        cancelled: Arc<AtomicBool>,
        reply: Reply,
    ) {
        if let Err(error) = self.can_send_file() {
            self.file_share_event(&self.lane.chat, "failed", "state");
            let _ = reply.send(Err(error));
            return;
        }
        self.files.pending.retain(|flag| !flag.load(Ordering::Acquire));
        if self.files.pending.len() >= 4 {
            self.file_share_event(&self.lane.chat, "failed", "capacity");
            let _ = reply.send(Err(anyhow!("too many pending file shares")));
            return;
        }
        // 群内开始复制即进入工具工作，复制失败后也不能恢复旧进度。
        if let Some(active) = self.active_delivery.as_mut() {
            active.invalidate_reply_before_pass();
        }
        self.files.pending.push(cancelled.clone());
        let epoch = self.files.epoch;
        let lane = self.lane.clone();
        let delivery_id = self.active_delivery.as_ref().map(|active| active.delivery.id.clone());
        let root = files::root(&lane.chat);
        let cwd = PathBuf::from(&self.cfg.cwd);
        let tx = self.files.tx.clone();
        tokio::spawn(async move {
            let flag = cancelled.clone();
            let result =
                tokio::task::spawn_blocking(move || files::snapshot(&root, &cwd, &source, name.as_deref(), &flag))
                    .await;
            let result = result.map_err(anyhow::Error::from).and_then(std::convert::identity);
            let _ = tx.send(Completed { epoch, cancelled, lane, delivery_id, reply, result });
        });
    }

    /// 仅允许当前正常用户回合或仍在执行的群投递，委托和定时任务保持禁用。
    fn can_send_file(&self) -> Result<i64> {
        ensure!(!self.stop_requested, "turn is stopping");
        ensure!(
            self.active_request.is_none() && self.active_routine.is_none(),
            "file sharing is unavailable in delegated or routine turns"
        );
        if let Some(active) = &self.active_delivery {
            ensure!(
                self.hub
                    .store
                    .delivery(&active.delivery.id)?
                    .is_some_and(|d| d.status == crate::store::deliveries::Status::Started
                        && d.room_id == self.lane.chat
                        && d.target_bot_id == self.cfg.id),
                "group delivery ended"
            );
        } else {
            ensure!(self.lane.chat == self.cfg.id, "not a bot chat turn");
        }
        self.turn.ok_or_else(|| anyhow!("no turn is running: between turns, tell the user with send_message"))
    }

    /// Artifact 卡片继续仅限自己的聊天，不随群文件分享扩大权限。
    pub(super) fn post_artifact(&mut self, data: Value) -> Result<String> {
        ensure!(self.active_delivery.is_none(), "artifact cards are only available in the bot's own chat");
        let turn = self.can_send_file()?;
        let text = data["text"].as_str().unwrap_or_default().to_owned();
        self.close_seg();
        let entry = self.add(EntryKind::Agent, turn, data).ok_or_else(|| anyhow!("couldn't save the artifact card"))?;
        self.files.summary = Some(text);
        self.set_activity("Working…");
        Ok(entry.id)
    }

    /// 仅记录文件分享的聊天种类、结果和固定失败类别，不上传文件或投递标识。
    fn file_share_event(&self, chat: &str, outcome: &str, failure: &str) {
        crate::analytics::capture(&self.hub, crate::analytics::Event::FileShared,
            json!({"chat": if chat == self.cfg.id { "direct" } else { "group" }, "outcome": outcome, "failure": failure})
                .as_object().cloned().unwrap_or_default());
    }

    /// 复制完成后重新核对轮次，在与 Stop 共用的发布锁内核验群投递并入库。
    pub(super) fn finish_file(&mut self, completed: Completed) {
        self.files.pending.retain(|flag| !Arc::ptr_eq(flag, &completed.cancelled));
        let result = (|| {
            ensure!(
                completed.epoch == self.files.epoch
                    && completed.lane == self.lane
                    && completed.delivery_id == self.active_delivery.as_ref().map(|active| active.delivery.id.clone())
                    && !completed.cancelled.load(Ordering::Acquire)
                    && !completed.reply.is_closed(),
                "file sharing cancelled"
            );
            let turn = self.can_send_file()?;
            ensure!(self.hub.store.bot(&self.cfg.id)?.is_some_and(|bot| !bot.deleted), "bot was deleted");
            let mut snapshot = completed.result?;
            let text = format!("{} ({} bytes)", snapshot.meta.name, snapshot.meta.size);
            self.close_seg_for_work();
            let data = json!({"text": text, "final": true, "files": [snapshot.meta], "author": self.cfg.id});
            if let Some(id) = &completed.delivery_id {
                self.hub.publish_delivery_file(id, &self.cfg.id, &completed.lane, turn, &data)?;
            } else {
                self.add(EntryKind::Agent, turn, data).ok_or_else(|| anyhow!("couldn't save the file message"))?;
            }
            snapshot.commit();
            self.files.summary = Some(text);
            self.set_activity("Working…");
            Ok(())
        })();
        self.file_share_event(
            &completed.lane.chat,
            if result.is_ok() { "published" } else { "failed" },
            if result.is_ok() { "none" } else { "publication" },
        );
        let _ = completed.reply.send(result);
    }
}
