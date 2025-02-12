use crate::error::source_map_error;
use crate::moduleloader::{ModuleEntry, ModuleManager};
use crate::{
    prepend_script_source_header, AnyError, ScriptLoadState, ScriptState, ScriptStateStoreWrapper,
    ScriptsStateStore, ScriptsStateStoreHandle,
};
use deno_core::{Extension, RuntimeOptions, Snapshot};
use futures::{future::LocalBoxFuture, FutureExt};
use guild_logger::{GuildLogger, LogEntry};
use isolatecell::{IsolateCell, ManagedIsolate};
use serde::Serialize;
use std::convert::TryFrom;
use std::pin::Pin;
use std::{
    fmt::Display,
    rc::Rc,
    sync::{atomic::AtomicBool, Arc, RwLock as StdRwLock},
    task::{Context, Poll, Wake, Waker},
};
use stores::config::Script;
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tracing::{error, info, instrument};
use twilight_model::id::{marker::GuildMarker, Id};
use url::Url;
use v8::{CreateParams, HeapStatistics, IsolateHandle};
use vmthread::{CreateVmSuccess, ShutdownHandle, ShutdownReason, VmInterface};

#[derive(Debug, Clone)]
pub enum VmCommand {
    DispatchEvent(String, serde_json::Value, u64),
    LoadScript(Script),

    // note that this also reloads the runtime, shutting it down and starting it again
    // we send a message when that has been accomplished
    UnloadScripts(Vec<Script>),
    UpdateScript(Script),
    Restart(Vec<Script>),
}

#[derive(Debug)]
pub enum VmEvent {
    Shutdown(ShutdownReason),
    DispatchedEvent(u64),
    VmFinished,
}

#[derive(Clone, Copy, Debug)]
pub enum VmRole {
    Main,
    Pack(u64),
}

pub type GuildVmEvent = (Id<GuildMarker>, VmRole, VmEvent);

#[derive(Serialize)]
struct ScriptDispatchData {
    name: String,
    data: serde_json::Value,
}

pub struct Vm {
    ctx: VmContext,
    runtime: ManagedIsolate,

    rx: UnboundedReceiver<VmCommand>,
    tx: UnboundedSender<GuildVmEvent>,

    script_store: ScriptsStateStoreHandle,

    timeout_handle: VmShutdownHandle,
    guild_logger: GuildLogger,

    isolate_cell: Rc<IsolateCell>,

    extension_factory: ExtensionFactory,
    module_manager: Rc<ModuleManager>,

    wakeup_rx: UnboundedReceiver<()>,
}

#[derive(Debug, Clone)]
pub struct VmContext {
    pub guild_id: Id<GuildMarker>,
    pub role: VmRole,
}

impl Vm {
    async fn create_run(
        create_req: CreateRt,
        timeout_handle: VmShutdownHandle,
        isolate_cell: Rc<IsolateCell>,
        wakeup_rx: UnboundedReceiver<()>,
    ) {
        let script_store = ScriptsStateStore::new_rc();

        let module_manager = Rc::new(ModuleManager {
            module_map: create_req.extension_modules,
            guild_scripts: script_store.clone(),
        });

        let sandbox = Self::create_isolate(
            &create_req.extension_factory,
            module_manager.clone(),
            script_store.clone(),
            timeout_handle.clone(),
        );

        let mut rt = Self {
            guild_logger: create_req.guild_logger,
            ctx: create_req.ctx,
            rx: create_req.rx,
            tx: create_req.tx,
            script_store,

            timeout_handle,
            isolate_cell,
            runtime: sandbox,
            extension_factory: create_req.extension_factory,
            module_manager,
            wakeup_rx,
        };

        rt.guild_logger.log(LogEntry::info(
            rt.ctx.guild_id,
            "starting fresh guild vm...".to_string(),
        ));

        rt.emit_isolate_handle();

        for script in &create_req.load_scripts {
            rt.compile_script(script.clone());
        }

        for script in create_req.load_scripts {
            rt.run_script(script.id).await;
        }

        rt.run().await;
    }

    fn create_isolate(
        extension_factory: &ExtensionFactory,
        module_manager: Rc<ModuleManager>,
        script_load_states: ScriptsStateStoreHandle,
        shutdown_handle: VmShutdownHandle,
    ) -> ManagedIsolate {
        // let create_err_fn = create_error_fn(script_load_states.clone());

        let mut extensions = extension_factory();
        let cloned_load_states = script_load_states.clone();
        extensions.insert(
            0,
            Extension::builder("bl_core_rt")
                .js(deno_core::include_js_files!(
                  prefix "bl:core",
                  "botloader-core-rt.js",
                ))
                .state(move |op| {
                    op.put(cloned_load_states.clone());
                    Ok(())
                })
                .build(),
        );

        let options = RuntimeOptions {
            extensions_with_js: extensions,
            module_loader: Some(module_manager),
            get_error_class_fn: Some(&|err| {
                deno_core::error::get_custom_error_class(err).unwrap_or("Error")
            }),
            // yeah i have no idea what these values needs to be aligned to, but this seems to work so whatever
            // if it breaks when you update deno or v8 try different values until it works, if only they'd document the alignment requirements somewhere...
            create_params: Some(
                CreateParams::default()
                    .heap_limits(512 * 1024, 60 * 512 * 1024)
                    .allow_atomics_wait(false),
            ),
            startup_snapshot: Some(Snapshot::Static(crate::BOTLOADER_CORE_SNAPSHOT)),
            // js_error_create_fn: Some(create_err_fn),
            source_map_getter: Some(Box::new(ScriptStateStoreWrapper(script_load_states))),
            ..Default::default()
        };

        let another_handle = shutdown_handle.clone();
        ManagedIsolate::new_with_oom_handler_and_state(
            options,
            move |current, initial| {
                info!(
                    "near heap limit: current: {}, initial: {}",
                    current, initial
                );
                shutdown_handle.shutdown_vm(ShutdownReason::OutOfMemory, true);
                if current != initial {
                    current
                } else {
                    current + initial
                }
            },
            another_handle,
        )
    }

    fn emit_isolate_handle(&mut self) {
        let handle = {
            let mut rt = self.isolate_cell.enter_isolate(&mut self.runtime);
            rt.v8_isolate().thread_safe_handle()
        };

        let mut th = self.timeout_handle.inner.write().unwrap();
        th.isolate_handle = Some(handle);
    }

    pub async fn run(&mut self) {
        self.emit_isolate_handle();

        info!("running runtime");
        self.guild_logger.log(LogEntry::info(
            self.ctx.guild_id,
            "guild vm started".to_string(),
        ));

        let mut completed = false;
        while !self.check_terminated() {
            let fut = TickFuture {
                rx: &mut self.rx,
                rt: &mut self.runtime,
                cell: &self.isolate_cell,
                wakeup: &mut self.wakeup_rx,
                completed,
            };

            completed = false;

            match fut.await {
                TickResult::Command(Some(cmd)) => {
                    self.handle_cmd(cmd).await;
                }
                TickResult::Command(None) => {
                    // sender was dropped, shut ourselves down?
                }
                TickResult::Continue => {}
                TickResult::VmError(e) => {
                    self.guild_logger.log(LogEntry::error(
                        self.ctx.guild_id,
                        format!(
                            "Script error occurred: {}",
                            source_map_error(&self.script_store, e)
                        ),
                    ));
                }
                TickResult::Completed => {
                    let _ = self
                        .tx
                        .send((self.ctx.guild_id, self.ctx.role, VmEvent::VmFinished));
                    completed = true;
                }
            }
        }

        info!("terminating runtime for guild");

        let shutdown_reason = {
            self.timeout_handle
                .inner
                .read()
                .unwrap()
                .shutdown_reason
                .clone()
        };

        if let Some(ShutdownReason::ThreadTermination) = shutdown_reason {
            // cleanly finish the futures
            self.stop_vm().await;
        }

        self.tx
            .send((
                self.ctx.guild_id,
                self.ctx.role,
                VmEvent::Shutdown(if let Some(reason) = shutdown_reason {
                    reason
                } else {
                    ShutdownReason::Unknown
                }),
            ))
            .unwrap();
    }

    fn check_terminated(&mut self) -> bool {
        self.timeout_handle
            .terminated
            .load(std::sync::atomic::Ordering::SeqCst)
    }

    async fn handle_cmd(&mut self, cmd: VmCommand) {
        match cmd {
            VmCommand::Restart(new_scripts) => {
                self.restart(new_scripts).await;
            }
            VmCommand::DispatchEvent(name, evt, evt_id) => self.dispatch_event(&name, &evt, evt_id),
            VmCommand::LoadScript(script) => {
                if let Some(script) = self.compile_script(script) {
                    self.run_script(script.script.id).await
                }
            }
            VmCommand::UpdateScript(script) => {
                let mut cloned_scripts = self
                    .script_store
                    .borrow()
                    .scripts
                    .iter()
                    .map(|v| v.script.clone())
                    .collect::<Vec<_>>();

                let mut need_reset = false;
                for old in &mut cloned_scripts {
                    if old.id == script.id {
                        *old = script.clone();
                        need_reset = true;
                    }
                }

                if need_reset {
                    self.restart(cloned_scripts).await;
                }
            }
            VmCommand::UnloadScripts(scripts) => {
                let new_scripts = self
                    .script_store
                    .borrow()
                    .scripts
                    .iter()
                    .filter_map(|sc| {
                        if !scripts.iter().any(|isc| isc.id == sc.script.id) {
                            Some(sc.script.clone())
                        } else {
                            None
                        }
                    })
                    .collect::<Vec<_>>();

                self.restart(new_scripts).await;
            }
        };
        // self._dump_heap_stats();
    }

    #[instrument(skip(self, script))]
    fn compile_script(&self, script: Script) -> Option<ScriptState> {
        let mut script_store = self.script_store.borrow_mut();

        let name = script.name.clone();
        match script_store.compile_add_script(script) {
            Ok(compiled) => Some(compiled),
            Err(e) => {
                self.guild_logger.log(LogEntry::error(
                    self.ctx.guild_id,
                    format!("Script compilation failed for {name}.ts: {e}"),
                ));
                None
            }
        }
    }

    #[instrument(skip(self))]
    async fn run_script(&mut self, script_id: u64) {
        let script = {
            let borrow = self.script_store.borrow();
            if matches!(borrow.is_failed_or_loaded(script_id), Some(true)) {
                info!("script: was already loaded or failed, skipping");
                return;
            }

            if let Some(script) = borrow.get_script(script_id) {
                script.clone()
            } else {
                error!("tried to load non-existant script");
                return;
            }
        };

        {
            self.script_store
                .borrow_mut()
                .set_state(script_id, ScriptLoadState::Loaded);
        }

        let eval_res = {
            let mut rt = self.isolate_cell.enter_isolate(&mut self.runtime);

            let parsed_uri =
                Url::parse(format!("file:///guild_scripts/{}.js", script.script.name).as_str())
                    .unwrap();

            let fut = rt.load_side_module(
                &parsed_uri,
                Some(prepend_script_source_header(
                    &script.compiled.output,
                    Some(&script.script),
                )),
            );

            // Yes this is very hacky, we should have a proper solution for this at some point.
            //
            // Why is this needed? because we can't hold the IsolateGuard across an await
            // this future should resolve instantly because our module loader has no awaits in it
            // and does no io.
            //
            // this might very well break in the future when we update to a newer version of deno
            // but hopefully it's caught before production.
            let res = {
                let mut pinned = Box::pin(fut);
                let waker: Waker = Arc::new(NoOpWaker).into();
                let mut cx = Context::from_waker(&waker);
                match pinned.poll_unpin(&mut cx) {
                    Poll::Pending => panic!("Future should resolve instantly!"),
                    Poll::Ready(v) => v,
                }
            };

            res.map(|id| rt.mod_evaluate(id))
        };

        match eval_res {
            Err(e) => {
                self.log_guild_err(e);
                self.script_store
                    .borrow_mut()
                    .set_state(script_id, ScriptLoadState::Failed);
            }
            Ok(rcv) => {
                self.complete_module_eval(rcv).await;
            }
        }
    }

    fn dispatch_event<P>(&mut self, name: &str, args: &P, evt_id: u64)
    where
        P: Serialize,
    {
        let _ = self.tx.send((
            self.ctx.guild_id,
            self.ctx.role,
            VmEvent::DispatchedEvent(evt_id),
        ));

        let data = ScriptDispatchData {
            data: serde_json::to_value(args).unwrap(),
            name: name.to_string(),
        };

        let mut rt = self.isolate_cell.enter_isolate(&mut self.runtime);
        let global_ctx = rt.global_context();
        let ctx = global_ctx.open(rt.v8_isolate());

        let mut scope = rt.handle_scope();
        let globals = ctx.global(&mut scope);

        let core_obj: v8::Local<v8::Object> =
            if let Some(obj) = Self::get_property(&mut scope, globals, "BotloaderCore") {
                if let Ok(v) = TryFrom::try_from(obj) {
                    v
                } else {
                    error!("BotloaderCore is not an object, unable to dispatch events");
                    return;
                }
            } else {
                error!("BotloaderCore global not found, unable to dispatch events");
                return;
            };

        let dispatch_fn: v8::Local<v8::Function> = if let Some(field) =
            Self::get_property(&mut scope, core_obj, "dispatchWrapper")
        {
            if let Ok(v) = TryFrom::try_from(field) {
                v
            } else {
                error!(
                    "BotloaderCore.dispatchWrapper is not a function, unable to dispatch events"
                );
                return;
            }
        } else {
            error!("BotloaderCore.dispatchWrapper not defined, unable to dispatch events");
            return;
        };

        let v = serde_v8::to_v8(&mut scope, &data).unwrap();
        let _ = dispatch_fn.call(&mut scope, globals.into(), &[v]);
    }

    fn get_property<'a>(
        scope: &mut v8::HandleScope<'a>,
        object: v8::Local<v8::Object>,
        key: &str,
    ) -> Option<v8::Local<'a, v8::Value>> {
        let key = v8::String::new(scope, key).unwrap();
        object.get(scope, key.into())
    }

    fn _dump_heap_stats(&mut self) {
        let mut rt = self.isolate_cell.enter_isolate(&mut self.runtime);
        let iso = rt.v8_isolate();
        let mut stats = HeapStatistics::default();
        iso.get_heap_statistics(&mut stats);
        dbg!(stats.total_heap_size());
        dbg!(stats.total_heap_size_executable());
        dbg!(stats.total_physical_size());
        dbg!(stats.total_available_size());
        dbg!(stats.total_global_handles_size());
        dbg!(stats.used_global_handles_size());
        dbg!(stats.used_heap_size());
        dbg!(stats.heap_size_limit());
        dbg!(stats.malloced_memory());
        dbg!(stats.external_memory());

        let policy = iso.get_microtasks_policy();
        dbg!(policy);
        // iso.low_memory_notification();
    }

    #[instrument(skip(self))]
    async fn stop_vm(&mut self) {
        // complete the event loop and extract our core data (script event receiver)
        // TODO: we could potentially have some long running futures
        // so maybe call a function that cancels all long running futures or something?
        // or at the very least have a timeout?
        if tokio::time::timeout(
            tokio::time::Duration::from_secs(15),
            self.run_until_completion(),
        )
        .await
        .is_err()
        {
            self.guild_logger.log(LogEntry::error(
                self.ctx.guild_id,
                "shutting down your vm timed out after 15 sec, cancelling all pending promises \
                 and force-shutting down now instead..."
                    .to_string(),
            ));
        }
    }

    async fn run_until_completion(&mut self) {
        loop {
            let fut = RunUntilCompletion {
                cell: &self.isolate_cell,
                rt: &mut self.runtime,
            };

            if let Err(err) = fut.await {
                self.log_guild_err(err);
            } else {
                return;
            }
        }
    }

    async fn complete_module_eval(
        &mut self,
        mut rcv: futures::channel::oneshot::Receiver<Result<(), AnyError>>,
    ) {
        loop {
            let fut = CompleteModuleEval {
                cell: &self.isolate_cell,
                rt: &mut self.runtime,
                rcv: &mut rcv,
            };

            match fut.await {
                Err(err) => {
                    self.log_guild_err(err);
                }
                Ok(_) => break,
            }
        }
    }

    fn log_guild_err(&self, err: AnyError) {
        self.guild_logger.log(LogEntry::error(
            self.ctx.guild_id,
            format!(
                "Script error occurred: {}",
                source_map_error(&self.script_store, err)
            ),
        ));
    }

    async fn restart(&mut self, new_scripts: Vec<Script>) {
        self.guild_logger.log(LogEntry::info(
            self.ctx.guild_id,
            "restarting guild vm...".to_string(),
        ));

        self.stop_vm().await;

        // create a new sandbox
        {
            let mut borrow = self.script_store.borrow_mut();
            borrow.clear();
        };

        for script in &new_scripts {
            self.compile_script(script.clone());
        }

        let new_rt = Self::create_isolate(
            &self.extension_factory,
            self.module_manager.clone(),
            self.script_store.clone(),
            self.timeout_handle.clone(),
        );

        self.runtime = new_rt;
        self.emit_isolate_handle();

        for script in new_scripts {
            self.run_script(script.id).await;
        }

        self.guild_logger.log(LogEntry::info(
            self.ctx.guild_id,
            "vm restarted".to_string(),
        ));
    }
}

pub enum TickResult {
    VmError(AnyError),
    Completed,
    Command(Option<VmCommand>),
    Continue,
}

struct TickFuture<'a> {
    rx: &'a mut UnboundedReceiver<VmCommand>,
    rt: &'a mut ManagedIsolate,
    cell: &'a IsolateCell,
    wakeup: &'a mut UnboundedReceiver<()>,
    completed: bool,
}

// Future which drives the js event loop while at the same time retrieving commands
impl<'a> core::future::Future for TickFuture<'a> {
    type Output = TickResult;

    fn poll(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        let completed = self.completed;
        if self.wakeup.poll_recv(cx).is_ready() {
            return Poll::Ready(TickResult::Continue);
        }

        if let Poll::Ready(opt) = self.rx.poll_recv(cx) {
            return Poll::Ready(TickResult::Command(opt));
        }

        let mut rt = self.cell.enter_isolate(self.rt);

        // if !self.completed{
        // }

        match rt.poll_event_loop(cx, false) {
            Poll::Pending => {
                // let state_rc = rt.op_state();
                // let op_state = state_rc.borrow();
                // let pending_state = op_state.borrow::<PendingDispatchHandlers>();

                // if pending_state.pending == 0 {
                //     info!("force killed vm that was ready from non-important handlers");
                //     return Poll::Ready(TickResult::Completed);
                // }
                Poll::Pending
            }
            Poll::Ready(Ok(_)) => {
                if completed {
                    Poll::Pending
                } else {
                    Poll::Ready(TickResult::Completed)
                }
            }
            Poll::Ready(Err(e)) => Poll::Ready(TickResult::VmError(e)),
        }
    }
}

// future that drives the vm to completion, acquiring the isolate guard when needed
struct RunUntilCompletion<'a> {
    rt: &'a mut ManagedIsolate,
    cell: &'a IsolateCell,
}

impl<'a> core::future::Future for RunUntilCompletion<'a> {
    type Output = Result<(), AnyError>;

    fn poll(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        let mut rt = self.cell.enter_isolate(self.rt);

        match rt.poll_event_loop(cx, false) {
            // Poll::Pending => {
            //     let state_rc = rt.op_state();
            //     let op_state = state_rc.borrow();
            //     let pending_state = op_state.borrow::<PendingDispatchHandlers>();

            //     if pending_state.pending == 0 {
            //         info!("force killed vm that was ready from non-important handlers");
            //         Poll::Ready(Ok(()))
            //     } else {
            //         Poll::Pending
            //     }
            // }
            Poll::Pending => Poll::Pending,
            Poll::Ready(Ok(_)) => Poll::Ready(Ok(())),
            Poll::Ready(Err(e)) => Poll::Ready(Err(e)),
        }
    }
}

// future that drives the vm to completion, acquiring the isolate guard when needed
struct CompleteModuleEval<'a, 'b> {
    rt: &'a mut ManagedIsolate,
    cell: &'a IsolateCell,
    rcv: &'b mut futures::channel::oneshot::Receiver<Result<(), AnyError>>,
}

impl<'a, 'b> core::future::Future for CompleteModuleEval<'a, 'b> {
    type Output = Result<(), AnyError>;

    fn poll(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        let pinned = Pin::new(&mut self.rcv);
        match pinned.poll(cx) {
            Poll::Ready(res) => {
                return Poll::Ready(if let Ok(inner) = res { inner } else { Ok(()) })
            }
            Poll::Pending => {}
        }

        {
            let mut rt = self.cell.enter_isolate(self.rt);

            match rt.poll_event_loop(cx, false) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
                Poll::Ready(_) => {}
            }
        }

        // we might have gotten a result on the channel after polling the event loop
        let pinned = Pin::new(&mut self.rcv);
        if let Poll::Ready(r) = pinned.poll(cx) {
            return Poll::Ready(if let Ok(inner) = r { inner } else { Ok(()) });
        }

        Poll::Ready(Ok(()))
    }
}

impl VmInterface for Vm {
    type BuildDesc = CreateRt;

    type Future = LocalBoxFuture<'static, ()>;

    type VmId = RtId;

    fn create_vm(
        b: Self::BuildDesc,
        isolate_cell: Rc<IsolateCell>,
    ) -> vmthread::VmCreateResult<Self::VmId, Self::Future, Self::ShutdownHandle> {
        let (wakeup_tx, wakeup_rx) = mpsc::unbounded_channel();
        let shutdown_handle = VmShutdownHandle {
            terminated: Arc::new(AtomicBool::new(false)),
            inner: Arc::new(StdRwLock::new(ShutdownHandleInner {
                isolate_handle: None,
                shutdown_reason: None,
            })),
            wakeup: wakeup_tx,
        };
        let guild_id = b.ctx.guild_id;
        let id = RtId {
            guild_id,
            role: b.ctx.role,
        };

        let thandle_clone = shutdown_handle.clone();

        let fut = Box::pin(async move {
            let local_set = tokio::task::LocalSet::new();
            tracing_futures::Instrument::instrument(
                local_set.run_until(Vm::create_run(b, thandle_clone, isolate_cell, wakeup_rx)),
                tracing::info_span!("vm", guild_id = %guild_id),
            )
            .await;
        });

        Ok(CreateVmSuccess {
            future: fut,
            id,
            shutdown_handle,
        })
    }

    type ShutdownHandle = VmShutdownHandle;
}

#[derive(Clone)]
pub struct VmShutdownHandle {
    terminated: Arc<AtomicBool>,
    inner: Arc<StdRwLock<ShutdownHandleInner>>,
    wakeup: mpsc::UnboundedSender<()>,
}

impl ShutdownHandle for VmShutdownHandle {
    fn shutdown_vm(&self, reason: ShutdownReason, force: bool) {
        let mut inner = self.inner.write().unwrap();
        inner.shutdown_reason = Some(reason);
        if let Some(iso_handle) = &inner.isolate_handle {
            self.terminated
                .store(true, std::sync::atomic::Ordering::SeqCst);

            if force {
                iso_handle.terminate_execution();
            }
        } else {
            inner.shutdown_reason = None;
        }

        // trigger a shutdown check if we weren't in the js runtime
        self.wakeup.send(()).ok();
    }
}

struct ShutdownHandleInner {
    shutdown_reason: Option<ShutdownReason>,
    isolate_handle: Option<IsolateHandle>,
}

pub struct CreateRt {
    pub guild_logger: GuildLogger,
    pub rx: UnboundedReceiver<VmCommand>,
    pub tx: UnboundedSender<GuildVmEvent>,
    pub ctx: VmContext,
    pub load_scripts: Vec<Script>,
    pub extension_factory: ExtensionFactory,
    pub extension_modules: Vec<ModuleEntry>,
}

type ExtensionFactory = Box<dyn Fn() -> Vec<Extension> + Send>;

#[derive(Clone)]
pub struct RtId {
    guild_id: Id<GuildMarker>,
    role: VmRole,
}

impl Display for RtId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_fmt(format_args!(
            "Isolate (guild_id: {}, role: {:?})",
            self.guild_id, self.role
        ))
    }
}

pub fn in_mem_source_load_fn(src: &'static str) -> Box<dyn Fn() -> Result<String, AnyError>> {
    Box::new(move || Ok(src.to_string()))
}

struct NoOpWaker;

impl Wake for NoOpWaker {
    fn wake(self: Arc<Self>) {}
}
