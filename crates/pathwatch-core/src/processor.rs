//! The processor: everything that turns collector messages into what the
//! dashboard shows. It owns the history and the incident state; reading a
//! snapshot from it changes nothing.

use crate::engine::{Engine, HistoryLimits, StreamInfo};
use crate::incident::{IncidentConfig, IncidentTracker};
use crate::model::{Msg, Record};
use crate::snapshot::{self, ProviderMeta, Snapshot, StreamMeta, ViewSpec};

pub struct Processor {
    pub engine: Engine,
    pub incidents: IncidentTracker,
}

impl Processor {
    pub fn new(streams: Vec<StreamInfo>, limits: HistoryLimits) -> Self {
        Self::with_incidents(streams, limits, IncidentConfig::default())
    }

    /// As [`Self::new`], with incident thresholds from configuration.
    pub fn with_incidents(streams: Vec<StreamInfo>, limits: HistoryLimits, incidents: IncidentConfig) -> Self {
        let incidents = IncidentTracker::with_config(streams.iter().map(|s| s.protocol), incidents);
        Self { engine: Engine::new(streams, limits), incidents }
    }

    pub fn ingest(&mut self, msg: &Msg) {
        self.engine.ingest(msg);
        match msg {
            Msg::Record(Record::Observation(o)) => {
                if let Some(idx) = self.engine.stream_index(&o.stream) {
                    self.incidents.on_observation(idx, o);
                }
            }
            Msg::Record(Record::Gap(_)) => self.incidents.on_gap(),
            // A reload may have added streams; the engine has them already.
            Msg::Record(Record::Config(_)) => {
                let known = self.incidents.stream_count();
                self.incidents.add_streams((known..self.engine.stream_count()).map(|i| self.engine.stream_info(i).protocol));
            }
            _ => {}
        }
    }

    pub fn snapshot(&self, providers: &[ProviderMeta], streams: &[StreamMeta], view: &ViewSpec, now_ns: u64) -> Snapshot {
        snapshot::build(&self.engine, &self.incidents, providers, streams, view, now_ns)
    }
}
