use std::{ops::Add, sync::Arc};

use chrono::{DateTime, Utc};
use runtime_models::internal::script::ScriptMeta;
use stores::timers::ScheduledTask;
use tracing::{error, info};
use twilight_model::id::{marker::GuildMarker, Id};

use crate::scheduler;

pub struct Manager {
    storage: Arc<dyn scheduler::Store>,
    guild_id: Id<GuildMarker>,

    // outer option: none if not fetched, some if fetched
    // inner: none if no tasks remaining
    next_task_time: Option<Option<DateTime<Utc>>>,
    pending: Vec<u64>,
    task_names: Vec<String>,
}

impl Manager {
    pub fn new(guild_id: Id<GuildMarker>, storage: Arc<dyn scheduler::Store>) -> Self {
        Self {
            storage,
            guild_id,
            next_task_time: None,
            pending: Vec::new(),
            task_names: Vec::new(),
        }
    }

    pub async fn next_action(&mut self) -> NextAction {
        if self.next_task_time.is_none() {
            // fetch
            match self
                .storage
                .get_next_task_time(self.guild_id, &self.pending, &self.task_names)
                .await
            {
                Ok(v) => {
                    self.next_task_time = Some(v);
                }
                Err(err) => {
                    error!(%err, "failed fetching next task time");
                    return NextAction::Wait(Utc::now().add(chrono::Duration::seconds(10)));
                }
            }
        };

        match self.next_task_time {
            None => unreachable!(),
            Some(None) => NextAction::None,
            Some(Some(t)) => {
                if Utc::now() > t {
                    // trigger some tasks
                    match self
                        .storage
                        .get_triggered_tasks(
                            self.guild_id,
                            Utc::now(),
                            &self.pending,
                            &self.task_names,
                        )
                        .await
                    {
                        Ok(v) => {
                            for task in &v {
                                self.pending.push(task.id);
                            }
                            info!("pending tasks: {}", self.pending.len());
                            self.clear_next();
                            NextAction::Run(v)
                        }
                        Err(err) => {
                            error!(%err, "failed fetching triggered tasks time");
                            NextAction::Wait(Utc::now().add(chrono::Duration::seconds(10)))
                        }
                    }
                } else {
                    NextAction::Wait(t)
                }
            }
        }
    }

    pub async fn ack_triggered_task(&mut self, id: u64) {
        if let Some(index) =
            self.pending
                .iter()
                .enumerate()
                .find_map(|(i, v)| if *v == id { Some(i) } else { None })
        {
            self.pending.swap_remove(index);
        }

        loop {
            match self.storage.del_task_by_id(self.guild_id, id).await {
                Ok(_) => return,
                Err(err) => {
                    error!(%err, "failed deleting task");
                    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                }
            }
        }
    }

    // pub async fn failed_ack_pending(&mut self, id: u64) {
    //     if let Some(index) =
    //         self.pending
    //             .iter()
    //             .enumerate()
    //             .find_map(|(i, v)| if *v == id { Some(i) } else { None })
    //     {
    //         self.pending.swap_remove(index);
    //     }

    //     self.clear_next();
    // }

    pub fn clear_pending(&mut self) {
        info!("cleared pending");
        self.pending.clear();
    }

    pub fn clear_next(&mut self) {
        self.next_task_time = None;
    }

    pub fn clear_task_names(&mut self) {
        self.task_names.clear();
    }

    pub fn script_started(&mut self, meta: &ScriptMeta) {
        for name in &meta.task_names {
            if self.task_names.contains(name) {
                continue;
            }
            self.task_names.push(name.clone());
        }

        self.clear_next();
    }
}

pub type NextAction = crate::guild_handler::NextTimerAction<Vec<ScheduledTask>>;
