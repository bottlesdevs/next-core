use std::fs;

use uuid::Uuid;

use crate::{Operation, error::Error};

use super::BottleManager;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeleteProgress {
    Stopping,
    Removing,
}

impl BottleManager {
    pub fn delete(&self, id: Uuid) -> Operation<(), DeleteProgress> {
        let manager = self.clone();
        self.context
            .spawn(move |progress, cancellation| async move {
                let bottle = manager.open(id).await?;
                progress.send_replace(Some(DeleteProgress::Stopping));
                bottle.stop().await?;
                if cancellation.is_cancelled() {
                    return Err(Error::Cancelled);
                }
                progress.send_replace(Some(DeleteProgress::Removing));
                let path = manager.context.directories().bottle(id);
                manager
                    .context
                    .spawn_blocking(move || {
                        fs::remove_dir_all(path)?;
                        Ok(())
                    })
                    .await?;
                bottle.mark_deleted();
                manager.cache.lock().await.remove(&id);
                Ok(())
            })
    }
}
