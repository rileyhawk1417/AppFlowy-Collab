use std::marker::PhantomData;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Weak};
use std::time::Duration;

use futures_util::SinkExt;
use tokio::spawn;
use tokio::sync::{mpsc, oneshot, watch, Mutex};
use tokio::time::{interval, Instant, Interval};
use tracing::{debug, trace};

use crate::cloud_storage::error::SyncError;
use crate::cloud_storage::msg::{CollabSinkMessage, MessageState, PendingMsgQueue};

pub const DEFAULT_SYNC_TIMEOUT: u64 = 2;
#[derive(Clone, Debug)]
pub enum SinkState {
  Init,
  /// The sink is syncing the messages to the remote.
  Syncing,
  /// All the messages are synced to the remote.
  Finished,
}

impl SinkState {
  #[allow(dead_code)]
  pub fn is_init(&self) -> bool {
    matches!(self, SinkState::Init)
  }
}

/// Use to sync the [Msg] to the remote.
pub struct CollabSink<Sink, Msg> {
  uid: i64,
  /// The [Sink] is used to send the messages to the remote. It might be a websocket sink or
  /// other sink that implements the [SinkExt] trait.
  sender: Arc<Mutex<Sink>>,

  /// The [PendingMsgQueue] is used to queue the messages that are waiting to be sent to the
  /// remote. It will merge the messages if possible.
  pending_msg_queue: Arc<parking_lot::Mutex<PendingMsgQueue<Msg>>>,
  msg_id_counter: Arc<dyn MsgIdCounter>,

  /// The [watch::Sender] is used to notify the [CollabSinkRunner] to process the pending messages.
  /// Sending `false` will stop the [CollabSinkRunner].
  notifier: Arc<watch::Sender<bool>>,
  config: SinkConfig,

  /// Stop the [IntervalRunner] if the sink strategy is [SinkStrategy::FixInterval].
  #[allow(dead_code)]
  interval_runner_stop_tx: Option<mpsc::Sender<()>>,

  /// Used to calculate the time interval between two messages. Only used when the sink strategy
  /// is [SinkStrategy::FixInterval].
  instant: Mutex<Instant>,
  state_notifier: Arc<watch::Sender<SinkState>>,
}

impl<Sink, Msg> Drop for CollabSink<Sink, Msg> {
  fn drop(&mut self) {
    let _ = self.notifier.send(true);
  }
}

impl<E, Sink, Msg> CollabSink<Sink, Msg>
where
  E: std::error::Error + Send + Sync + 'static,
  Sink: SinkExt<Msg, Error = E> + Send + Sync + Unpin + 'static,
  Msg: CollabSinkMessage,
{
  pub fn new<C>(
    uid: i64,
    sink: Sink,
    notifier: watch::Sender<bool>,
    sync_state_tx: watch::Sender<SinkState>,
    msg_id_counter: C,
    config: SinkConfig,
  ) -> Self
  where
    C: MsgIdCounter,
  {
    let notifier = Arc::new(notifier);
    let state_notifier = Arc::new(sync_state_tx);
    let sender = Arc::new(Mutex::new(sink));
    let pending_msg_queue = PendingMsgQueue::new();
    let pending_msg_queue = Arc::new(parking_lot::Mutex::new(pending_msg_queue));
    let msg_id_counter = Arc::new(msg_id_counter);
    //
    let instant = Mutex::new(Instant::now());
    let mut interval_runner_stop_tx = None;
    if let SinkStrategy::FixInterval(duration) = &config.strategy {
      let weak_notifier = Arc::downgrade(&notifier);
      let (tx, rx) = mpsc::channel(1);
      interval_runner_stop_tx = Some(tx);
      spawn(IntervalRunner::new(*duration).run(weak_notifier, rx));
    }
    Self {
      uid,
      sender,
      pending_msg_queue,
      msg_id_counter,
      notifier,
      state_notifier,
      config,
      instant,
      interval_runner_stop_tx,
    }
  }

  /// Put the message into the queue and notify the sink to process the next message.
  /// After the [Msg] was pushed into the [PendingMsgQueue]. The queue will pop the next msg base on
  /// its priority. And the message priority is determined by the [Msg] that implement the [Ord] and
  /// [PartialOrd] trait. Check out the [CollabMessage] for more details.
  ///
  pub fn queue_msg(&self, f: impl FnOnce(MsgId) -> Msg) {
    {
      let mut pending_msgs = self.pending_msg_queue.lock();
      let msg_id = self.msg_id_counter.next();
      let msg = f(msg_id);
      pending_msgs.push_msg(msg_id, msg);
      drop(pending_msgs);
    }

    self.notify();
  }

  pub fn remove_all_pending_msgs(&self) {
    self.pending_msg_queue.lock().clear();
  }

  /// Notify the sink to process the next message and mark the current message as done.
  pub async fn ack_msg(&self, object_id: &str, msg_id: MsgId) {
    trace!("receive {} message:{}", object_id, msg_id);
    if let Some(mut pending_msg) = self.pending_msg_queue.lock().peek_mut() {
      // In most cases, the msg_id of the pending_msg is the same as the passed-in msg_id. However,
      // due to network issues, the client might send multiple messages with the same msg_id.
      // Therefore, the msg_id might not always match the msg_id of the pending_msg.
      debug_assert!(
        pending_msg.msg_id() >= msg_id,
        "{}: pending msg_id: {}, receive msg:{}",
        object_id,
        pending_msg.msg_id(),
        msg_id
      );
      if pending_msg.msg_id() == msg_id {
        debug!("{} message:{} was sent", object_id, msg_id);
        pending_msg.set_state(MessageState::Done);
        self.notify();
      }
    }
  }

  async fn process_next_msg(&self) -> Result<(), SyncError> {
    // Check if the next message can be deferred. If not, try to send the message immediately. The
    // default value is true.
    let deferrable = self
      .pending_msg_queue
      .try_lock()
      .map(|pending_msgs| {
        pending_msgs
          .peek()
          .map(|msg| msg.get_msg().deferrable())
          .unwrap_or(true)
      })
      .unwrap_or(true);

    if !deferrable {
      self.try_send_msg_immediately().await;
      return Ok(());
    }

    // Check the elapsed time from the last message. Return if the elapsed time is less than
    // the fix interval.
    if let SinkStrategy::FixInterval(duration) = &self.config.strategy {
      let elapsed = self.instant.lock().await.elapsed();
      // trace!(
      //   "elapsed interval: {:?}, fix interval: {:?}",
      //   elapsed,
      //   duration
      // );
      if elapsed < *duration {
        return Ok(());
      }
    }

    // Reset the instant if the strategy is [SinkStrategy::FixInterval].
    if self.config.strategy.is_fix_interval() {
      *self.instant.lock().await = Instant::now();
    }

    self.try_send_msg_immediately().await;
    Ok(())
  }

  async fn try_send_msg_immediately(&self) -> Option<()> {
    let (tx, rx) = oneshot::channel();
    let collab_msg = {
      let (mut pending_msg_queue, mut sending_msg) = match self.pending_msg_queue.try_lock() {
        None => {
          // If acquire the lock failed, try to notify again after 100ms
          let weak_notifier = Arc::downgrade(&self.notifier);
          spawn(async move {
            interval(Duration::from_millis(100)).tick().await;
            if let Some(notifier) = weak_notifier.upgrade() {
              let _ = notifier.send(false);
            }
          });
          None
        },
        Some(mut pending_msg_queue) => pending_msg_queue
          .pop()
          .map(|sending_msg| (pending_msg_queue, sending_msg)),
      }?;
      if sending_msg.state().is_done() {
        // Notify to process the next pending message
        self.notify();
        return None;
      }

      // Do nothing if the message is still processing.
      if sending_msg.state().is_processing() {
        return None;
      }

      // If the message can merge other messages, try to merge the next message until the
      // message is not mergeable.
      if sending_msg.is_mergeable() {
        while let Some(pending_msg) = pending_msg_queue.pop() {
          debug!("Try merge collab message: {}", pending_msg.get_msg());

          if !sending_msg.merge(pending_msg) {
            break;
          }
        }
      }

      sending_msg.set_state(MessageState::Processing);
      sending_msg.set_ret(tx);
      if !sending_msg.is_init() {
        let _ = self.state_notifier.send(SinkState::Syncing);
      }
      let collab_msg = sending_msg.get_msg().clone();
      pending_msg_queue.push(sending_msg);
      collab_msg
    };

    let mut sender = self.sender.lock().await;
    tracing::debug!("[🙂Client {}]: {}", self.uid, collab_msg);
    sender.send(collab_msg).await.ok()?;
    // Wait for the message to be acked.
    // If the message is not acked within the timeout, resend the message.
    match tokio::time::timeout(self.config.timeout, rx).await {
      Ok(_) => {
        if let Some(mut pending_msgs) = self.pending_msg_queue.try_lock() {
          let pending_msg = pending_msgs.pop();
          trace!(
            "{} was sent, current pending messages: {}",
            pending_msg
              .map(|msg| msg.get_msg().to_string())
              .unwrap_or("".to_string()),
            pending_msgs.len()
          );
          if pending_msgs.is_empty() {
            if let Err(e) = self.state_notifier.send(SinkState::Finished) {
              tracing::error!("send sink state failed: {}", e);
            }
          }
        }
        self.notify()
      },
      Err(_) => {
        if let Some(mut pending_msg) = self.pending_msg_queue.lock().peek_mut() {
          pending_msg.set_state(MessageState::Timeout);
        }
        self.notify();
      },
    }
    None
  }

  /// Notify the sink to process the next message.
  pub(crate) fn notify(&self) {
    let _ = self.notifier.send(false);
  }

  /// Stop the sink.
  #[allow(dead_code)]
  fn stop(&self) {
    let _ = self.notifier.send(true);
  }
}

pub struct CollabSinkRunner<Msg>(PhantomData<Msg>);

impl<Msg> CollabSinkRunner<Msg> {
  /// The runner will stop if the [CollabSink] was dropped or the notifier was closed.
  pub async fn run<E, Sink>(
    weak_sink: Weak<CollabSink<Sink, Msg>>,
    mut notifier: watch::Receiver<bool>,
  ) where
    E: std::error::Error + Send + Sync + 'static,
    Sink: SinkExt<Msg, Error = E> + Send + Sync + Unpin + 'static,
    Msg: CollabSinkMessage,
  {
    if let Some(sink) = weak_sink.upgrade() {
      sink.notify();
    }
    loop {
      // stops the runner if the notifier was closed.
      if notifier.changed().await.is_err() {
        break;
      }

      // stops the runner if the value of notifier is `true`
      if *notifier.borrow() {
        break;
      }

      if let Some(sync_sink) = weak_sink.upgrade() {
        let _ = sync_sink.process_next_msg().await;
      } else {
        break;
      }
    }
  }
}

pub struct SinkConfig {
  /// `timeout` is the time to wait for the remote to ack the message. If the remote
  /// does not ack the message in time, the message will be sent again.
  pub timeout: Duration,
  /// `max_zip_size` is the maximum size of the messages to be merged.
  pub max_merge_size: usize,
  /// `strategy` is the strategy to send the messages.
  pub strategy: SinkStrategy,
}

impl SinkConfig {
  pub fn new() -> Self {
    Self::default()
  }
  pub fn with_timeout(mut self, secs: u64) -> Self {
    let timeout_duration = Duration::from_secs(secs);
    if let SinkStrategy::FixInterval(duration) = self.strategy {
      if timeout_duration < duration {
        tracing::warn!("The timeout duration should greater than the fix interval duration");
      }
    }
    self.timeout = timeout_duration;
    self
  }

  /// `with_max_merge_size` is the maximum size of the messages to be merged.
  #[allow(dead_code)]
  pub fn with_max_merge_size(mut self, max_merge_size: usize) -> Self {
    self.max_merge_size = max_merge_size;
    self
  }

  pub fn with_strategy(mut self, strategy: SinkStrategy) -> Self {
    if let SinkStrategy::FixInterval(duration) = strategy {
      if self.timeout < duration {
        tracing::warn!("The timeout duration should greater than the fix interval duration");
      }
    }
    self.strategy = strategy;
    self
  }
}

impl Default for SinkConfig {
  fn default() -> Self {
    Self {
      timeout: Duration::from_secs(DEFAULT_SYNC_TIMEOUT),
      max_merge_size: 4096,
      strategy: SinkStrategy::Asap,
    }
  }
}

pub enum SinkStrategy {
  /// Send the message as soon as possible.
  Asap,
  /// Send the message in a fixed interval.
  /// This can reduce the number of times the message is sent. Especially if using the AWS
  /// as the storage layer, the cost of sending the message is high. However, it may increase
  /// the latency of the message.
  FixInterval(Duration),
}

impl SinkStrategy {
  pub fn is_fix_interval(&self) -> bool {
    matches!(self, SinkStrategy::FixInterval(_))
  }
}

pub type MsgId = u64;

pub trait MsgIdCounter: Send + Sync + 'static {
  /// Get the next message id. The message id should be unique.
  fn next(&self) -> MsgId;
}

#[derive(Debug, Default)]
pub struct DefaultMsgIdCounter(Arc<AtomicU64>);

impl MsgIdCounter for DefaultMsgIdCounter {
  fn next(&self) -> MsgId {
    self.0.fetch_add(1, Ordering::SeqCst)
  }
}

struct IntervalRunner {
  interval: Option<Interval>,
}

impl IntervalRunner {
  fn new(duration: Duration) -> Self {
    Self {
      interval: Some(tokio::time::interval(duration)),
    }
  }
}

impl IntervalRunner {
  pub async fn run(mut self, sender: Weak<watch::Sender<bool>>, mut stop_rx: mpsc::Receiver<()>) {
    let mut interval = self
      .interval
      .take()
      .expect("Interval should only take once");
    loop {
      tokio::select! {
        _ = stop_rx.recv() => {
            break;
        },
        _ = interval.tick() => {
          if let Some(sender) = sender.upgrade() {
            let _ = sender.send(false);
          } else {
            break;
          }
        }
      }
    }
  }
}
