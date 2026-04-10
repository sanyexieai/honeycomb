use std::collections::VecDeque;

use serde::{Deserialize, Serialize};

use hc_claim::{ClaimError, NominationPolicy, ParticipationClaim, SpeakingGrant, select_winner};
use hc_protocol::protocol::RecordId;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RuntimeState {
    pub sessions: Vec<SessionRecord>,
    pub channels: Vec<ChannelRecord>,
    pub instances: Vec<InstanceRecord>,
    pub workers: Vec<WorkerRecord>,
    pub jobs: Vec<JobRecord>,
    pub messages: Vec<MessageRecord>,
    #[serde(default)]
    pub nominations: Vec<NominationRecord>,
    #[serde(default)]
    pub claims: Vec<ParticipationClaim>,
    #[serde(default)]
    pub speaking_grants: Vec<SpeakingGrant>,
    pub events: Vec<EventRecord>,
}

impl Default for RuntimeState {
    fn default() -> Self {
        Self {
            sessions: Vec::new(),
            channels: Vec::new(),
            instances: Vec::new(),
            workers: Vec::new(),
            jobs: Vec::new(),
            messages: Vec::new(),
            nominations: Vec::new(),
            claims: Vec::new(),
            speaking_grants: Vec::new(),
            events: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RuntimeNamespace {
    pub tenant_id: String,
    pub user_id: String,
}

impl RuntimeNamespace {
    pub fn new(tenant_id: impl Into<String>, user_id: impl Into<String>) -> Self {
        Self {
            tenant_id: tenant_id.into(),
            user_id: user_id.into(),
        }
    }

    pub fn local_default() -> Self {
        Self::new("local", "default")
    }
}

impl Default for RuntimeNamespace {
    fn default() -> Self {
        Self::local_default()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum RuntimeCommand {
    CreateSession {
        name: String,
        namespace: Option<RuntimeNamespace>,
    },
    CreateInstance {
        session_id: String,
        name: String,
        parent_instance_id: Option<String>,
    },
    CreateChannel {
        session_id: String,
        name: String,
    },
    JoinChannel {
        instance_id: String,
        channel_id: String,
    },
    LeaveChannel {
        instance_id: String,
        channel_id: String,
    },
    RenameInstance {
        instance_id: String,
        name: String,
    },
    PostMessage {
        session_id: String,
        from: String,
        route: MessageRoute,
        kind: MessageKind,
        body: String,
        reply_to: Option<String>,
    },
    SubmitParticipationClaim {
        claim: ParticipationClaim,
    },
    ResolveSpeakingGrant {
        message_id: String,
        round: u32,
    },
    SubmitRunRequest {
        instance_id: String,
        title: String,
        run_request: RunRequest,
    },
    UpdateJobState {
        job_id: String,
        next_state: JobState,
    },
    AppendJobEvent {
        job_id: String,
        kind: EventKind,
        payload: String,
    },
    PromoteJobToChildInstance {
        job_id: String,
        child_name: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum RuntimeCommandResult {
    Session(SessionRecord),
    Channel(ChannelRecord),
    Instance(InstanceRecord),
    Message(MessageRecord),
    Claim(ParticipationClaim),
    SpeakingGrant(Option<SpeakingGrant>),
    Job(JobRecord),
    Event(EventRecord),
    Ack,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum WorkerReport {
    JobStateChanged {
        job_id: String,
        state: JobState,
    },
    Stdout {
        job_id: String,
        chunk: String,
    },
    Stderr {
        job_id: String,
        chunk: String,
    },
    Exited {
        job_id: String,
        success: bool,
    },
}

#[derive(Debug, Default)]
pub struct RuntimeSupervisor {
    state: RuntimeState,
    command_queue: VecDeque<RuntimeCommand>,
    nomination_policy: NominationPolicy,
    next_session: u64,
    next_channel: u64,
    next_instance: u64,
    next_worker: u64,
    next_job: u64,
    next_message: u64,
    next_event: u64,
}

impl RuntimeSupervisor {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn from_state(state: RuntimeState) -> Self {
        Self {
            nomination_policy: NominationPolicy::default(),
            next_session: state.sessions.len() as u64,
            next_channel: state.channels.len() as u64,
            next_instance: state.instances.len() as u64,
            next_worker: state.workers.len() as u64,
            next_job: state.jobs.len() as u64,
            next_message: state.messages.len() as u64,
            next_event: state.events.len() as u64,
            state,
            command_queue: VecDeque::new(),
        }
    }

    pub fn state(&self) -> &RuntimeState {
        &self.state
    }

    pub fn nomination_policy(&self) -> &NominationPolicy {
        &self.nomination_policy
    }

    pub fn queued_command_count(&self) -> usize {
        self.command_queue.len()
    }

    pub fn session(&self, session_id: &str) -> Option<&SessionRecord> {
        self.state
            .sessions
            .iter()
            .find(|session| session.id == session_id)
    }

    pub fn instance(&self, instance_id: &str) -> Option<&InstanceRecord> {
        self.state
            .instances
            .iter()
            .find(|instance| instance.id == instance_id)
    }

    pub fn channel(&self, channel_id: &str) -> Option<&ChannelRecord> {
        self.state
            .channels
            .iter()
            .find(|channel| channel.id == channel_id)
    }

    pub fn job(&self, job_id: &str) -> Option<&JobRecord> {
        self.state.jobs.iter().find(|job| job.id == job_id)
    }

    pub fn worker(&self, worker_id: &str) -> Option<&WorkerRecord> {
        self.state
            .workers
            .iter()
            .find(|worker| worker.id == worker_id)
    }

    pub fn into_state(self) -> RuntimeState {
        self.state
    }

    pub fn enqueue_command(&mut self, command: RuntimeCommand) {
        self.command_queue.push_back(command);
    }

    pub fn step(&mut self) -> Option<Result<RuntimeCommandResult, RuntimeError>> {
        let command = self.command_queue.pop_front()?;
        Some(self.dispatch(command))
    }

    pub fn drain_commands(&mut self) -> Vec<Result<RuntimeCommandResult, RuntimeError>> {
        let mut results = Vec::new();
        while let Some(result) = self.step() {
            results.push(result);
        }
        results
    }

    pub fn drain_events(&mut self) -> Vec<EventRecord> {
        std::mem::take(&mut self.state.events)
    }

    pub fn events_since(&self, start_index: usize) -> &[EventRecord] {
        let len = self.state.events.len();
        let start = start_index.min(len);
        &self.state.events[start..]
    }

    pub fn dispatch(
        &mut self,
        command: RuntimeCommand,
    ) -> Result<RuntimeCommandResult, RuntimeError> {
        match command {
            RuntimeCommand::CreateSession { name, namespace } => Ok(
                RuntimeCommandResult::Session(match namespace {
                    Some(namespace) => self.create_session_in_namespace(name, namespace),
                    None => self.create_session(name),
                }),
            ),
            RuntimeCommand::CreateInstance {
                session_id,
                name,
                parent_instance_id,
            } => self
                .create_instance(&session_id, name, parent_instance_id)
                .map(RuntimeCommandResult::Instance),
            RuntimeCommand::CreateChannel { session_id, name } => self
                .create_channel(&session_id, name)
                .map(RuntimeCommandResult::Channel),
            RuntimeCommand::JoinChannel {
                instance_id,
                channel_id,
            } => self
                .join_channel(&instance_id, &channel_id)
                .map(RuntimeCommandResult::Instance),
            RuntimeCommand::LeaveChannel {
                instance_id,
                channel_id,
            } => self
                .leave_channel(&instance_id, &channel_id)
                .map(RuntimeCommandResult::Instance),
            RuntimeCommand::RenameInstance { instance_id, name } => self
                .rename_instance(&instance_id, name)
                .map(RuntimeCommandResult::Instance),
            RuntimeCommand::PostMessage {
                session_id,
                from,
                route,
                kind,
                body,
                reply_to,
            } => self
                .post_message(&session_id, &from, route, kind, body, reply_to)
                .map(RuntimeCommandResult::Message),
            RuntimeCommand::SubmitParticipationClaim { claim } => self
                .submit_participation_claim(claim)
                .map(RuntimeCommandResult::Claim),
            RuntimeCommand::ResolveSpeakingGrant { message_id, round } => self
                .resolve_speaking_grant(&message_id, round)
                .map(RuntimeCommandResult::SpeakingGrant),
            RuntimeCommand::SubmitRunRequest {
                instance_id,
                title,
                run_request,
            } => self
                .submit_run_request(&instance_id, title, run_request)
                .map(RuntimeCommandResult::Job),
            RuntimeCommand::UpdateJobState { job_id, next_state } => {
                self.update_job_state(&job_id, next_state)?;
                Ok(RuntimeCommandResult::Ack)
            }
            RuntimeCommand::AppendJobEvent {
                job_id,
                kind,
                payload,
            } => self
                .append_job_event(&job_id, kind, payload)
                .map(RuntimeCommandResult::Event),
            RuntimeCommand::PromoteJobToChildInstance { job_id, child_name } => self
                .promote_job_to_child_instance(&job_id, child_name)
                .map(RuntimeCommandResult::Instance),
        }
    }

    pub fn apply_worker_report(
        &mut self,
        report: WorkerReport,
    ) -> Result<Option<EventRecord>, RuntimeError> {
        match report {
            WorkerReport::JobStateChanged { job_id, state } => {
                self.update_job_state(&job_id, state)?;
                Ok(None)
            }
            WorkerReport::Stdout { job_id, chunk } => self
                .append_job_event(&job_id, EventKind::JobStdout, chunk)
                .map(Some),
            WorkerReport::Stderr { job_id, chunk } => self
                .append_job_event(&job_id, EventKind::JobStderr, chunk)
                .map(Some),
            WorkerReport::Exited { job_id, success } => {
                let state = if success {
                    JobState::Succeeded
                } else {
                    JobState::Failed
                };
                self.update_job_state(&job_id, state)?;
                Ok(None)
            }
        }
    }

    pub fn create_session(&mut self, name: impl Into<String>) -> SessionRecord {
        self.create_session_in_namespace(name, RuntimeNamespace::local_default())
    }

    pub fn create_session_in_namespace(
        &mut self,
        name: impl Into<String>,
        namespace: RuntimeNamespace,
    ) -> SessionRecord {
        let session = SessionRecord {
            id: self.next_id(IdKind::Session),
            name: name.into(),
            namespace,
            instance_ids: Vec::new(),
            channel_ids: Vec::new(),
        };
        let event_id = self.next_id(IdKind::Event);
        self.push_event(EventRecord {
            id: event_id,
            session_id: session.id.clone(),
            source: "runtime".to_owned(),
            target: None,
            job_id: None,
            kind: EventKind::SessionCreated,
            payload: session.name.clone(),
        });
        self.state.sessions.push(session.clone());
        session
    }

    pub fn create_instance(
        &mut self,
        session_id: &str,
        name: impl Into<String>,
        parent_instance_id: Option<String>,
    ) -> Result<InstanceRecord, RuntimeError> {
        let session_index = self
            .find_session_index(session_id)
            .ok_or_else(|| RuntimeError::session_not_found(session_id))?;

        if let Some(parent_id) = parent_instance_id.as_deref() {
            self.find_instance_index(parent_id)
                .ok_or_else(|| RuntimeError::instance_not_found(parent_id))?;
        }

        let instance = InstanceRecord {
            id: self.next_id(IdKind::Instance),
            session_id: session_id.to_owned(),
            namespace: self.state.sessions[session_index].namespace.clone(),
            name: name.into(),
            parent_instance_id: parent_instance_id.clone(),
            child_instance_ids: Vec::new(),
            channel_ids: Vec::new(),
            job_ids: Vec::new(),
            worker_ids: Vec::new(),
        };

        self.state.sessions[session_index]
            .instance_ids
            .push(instance.id.clone());

        if let Some(parent_id) = parent_instance_id {
            let parent_index = self
                .find_instance_index(&parent_id)
                .ok_or_else(|| RuntimeError::instance_not_found(&parent_id))?;
            self.state.instances[parent_index]
                .child_instance_ids
                .push(instance.id.clone());
        }

        let event_id = self.next_id(IdKind::Event);
        self.push_event(EventRecord {
            id: event_id,
            session_id: session_id.to_owned(),
            source: instance.id.clone(),
            target: None,
            job_id: None,
            kind: EventKind::InstanceCreated,
            payload: instance.name.clone(),
        });

        self.state.instances.push(instance.clone());
        Ok(instance)
    }

    pub fn create_channel(
        &mut self,
        session_id: &str,
        name: impl Into<String>,
    ) -> Result<ChannelRecord, RuntimeError> {
        let session_index = self
            .find_session_index(session_id)
            .ok_or_else(|| RuntimeError::session_not_found(session_id))?;

        let channel = ChannelRecord {
            id: self.next_id(IdKind::Channel),
            session_id: session_id.to_owned(),
            namespace: self.state.sessions[session_index].namespace.clone(),
            name: name.into(),
            member_instance_ids: Vec::new(),
        };

        self.state.sessions[session_index]
            .channel_ids
            .push(channel.id.clone());

        let event_id = self.next_id(IdKind::Event);
        self.push_event(EventRecord {
            id: event_id,
            session_id: session_id.to_owned(),
            source: "runtime".to_owned(),
            target: Some(channel.id.clone()),
            job_id: None,
            kind: EventKind::ChannelCreated,
            payload: channel.name.clone(),
        });

        self.state.channels.push(channel.clone());
        Ok(channel)
    }

    pub fn join_channel(
        &mut self,
        instance_id: &str,
        channel_id: &str,
    ) -> Result<InstanceRecord, RuntimeError> {
        let instance_index = self
            .find_instance_index(instance_id)
            .ok_or_else(|| RuntimeError::instance_not_found(instance_id))?;
        let channel_index = self
            .find_channel_index(channel_id)
            .ok_or_else(|| RuntimeError::channel_not_found(channel_id))?;

        let session_id = self.state.instances[instance_index].session_id.clone();
        if self.state.channels[channel_index].session_id != session_id {
            return Err(RuntimeError::channel_session_mismatch(
                channel_id, &session_id,
            ));
        }

        if !self.state.instances[instance_index]
            .channel_ids
            .iter()
            .any(|id| id == channel_id)
        {
            self.state.instances[instance_index]
                .channel_ids
                .push(channel_id.to_owned());
        }
        if !self.state.channels[channel_index]
            .member_instance_ids
            .iter()
            .any(|id| id == instance_id)
        {
            self.state.channels[channel_index]
                .member_instance_ids
                .push(instance_id.to_owned());
        }

        let instance = self.state.instances[instance_index].clone();
        let event_id = self.next_id(IdKind::Event);
        self.push_event(EventRecord {
            id: event_id,
            session_id,
            source: instance.id.clone(),
            target: Some(channel_id.to_owned()),
            job_id: None,
            kind: EventKind::ChannelJoined,
            payload: channel_id.to_owned(),
        });
        Ok(instance)
    }

    pub fn leave_channel(
        &mut self,
        instance_id: &str,
        channel_id: &str,
    ) -> Result<InstanceRecord, RuntimeError> {
        let instance_index = self
            .find_instance_index(instance_id)
            .ok_or_else(|| RuntimeError::instance_not_found(instance_id))?;
        let channel_index = self
            .find_channel_index(channel_id)
            .ok_or_else(|| RuntimeError::channel_not_found(channel_id))?;

        self.state.instances[instance_index]
            .channel_ids
            .retain(|id| id != channel_id);
        self.state.channels[channel_index]
            .member_instance_ids
            .retain(|id| id != instance_id);

        let instance = self.state.instances[instance_index].clone();
        let session_id = instance.session_id.clone();
        let event_id = self.next_id(IdKind::Event);
        self.push_event(EventRecord {
            id: event_id,
            session_id,
            source: instance.id.clone(),
            target: Some(channel_id.to_owned()),
            job_id: None,
            kind: EventKind::ChannelLeft,
            payload: channel_id.to_owned(),
        });
        Ok(instance)
    }

    pub fn post_message(
        &mut self,
        session_id: &str,
        from: &str,
        route: MessageRoute,
        kind: MessageKind,
        body: impl Into<String>,
        reply_to: Option<String>,
    ) -> Result<MessageRecord, RuntimeError> {
        self.ensure_instance_in_session(session_id, from)?;
        match &route {
            MessageRoute::Direct { to } => {
                self.ensure_instance_in_session(session_id, to)?;
            }
            MessageRoute::Broadcast => {}
            MessageRoute::Channel { channel_id } => {
                self.ensure_channel_in_session(session_id, channel_id)?;
                self.ensure_instance_in_channel(from, channel_id)?;
            }
        }

        let message = MessageRecord {
            id: self.next_id(IdKind::Message),
            session_id: session_id.to_owned(),
            from: from.to_owned(),
            route: route.clone(),
            kind,
            body: body.into(),
            reply_to,
        };

        self.state.messages.push(message.clone());
        let event_id = self.next_id(IdKind::Event);
        let target = match &message.route {
            MessageRoute::Direct { to } => Some(to.clone()),
            MessageRoute::Broadcast => None,
            MessageRoute::Channel { channel_id } => Some(channel_id.clone()),
        };
        self.push_event(EventRecord {
            id: event_id,
            session_id: session_id.to_owned(),
            source: from.to_owned(),
            target,
            job_id: None,
            kind: EventKind::MessagePosted,
            payload: message.body.clone(),
        });

        if message.kind == MessageKind::Chat
            && matches!(
                message.route,
                MessageRoute::Broadcast | MessageRoute::Channel { .. }
            )
        {
            let opening_round = self
                .nomination_policy
                .rounds
                .first()
                .map(|round| round.round)
                .unwrap_or(1);
            self.state.nominations.push(NominationRecord {
                message_id: message.id.clone(),
                session_id: session_id.to_owned(),
                route: message.route.clone(),
                current_round: opening_round,
                status: NominationStatus::Open,
            });

            let nomination_event_id = self.next_id(IdKind::Event);
            self.push_event(EventRecord {
                id: nomination_event_id,
                session_id: session_id.to_owned(),
                source: "runtime".to_owned(),
                target: Some(message.id.clone()),
                job_id: None,
                kind: EventKind::NominationOpened,
                payload: format!("round={opening_round}"),
            });
        }

        Ok(message)
    }

    pub fn rename_instance(
        &mut self,
        instance_id: &str,
        name: impl Into<String>,
    ) -> Result<InstanceRecord, RuntimeError> {
        let instance_index = self
            .find_instance_index(instance_id)
            .ok_or_else(|| RuntimeError::instance_not_found(instance_id))?;
        let session_id = self.state.instances[instance_index].session_id.clone();
        let name = name.into();
        self.state.instances[instance_index].name = name.clone();
        let instance = self.state.instances[instance_index].clone();

        let event_id = self.next_id(IdKind::Event);
        self.push_event(EventRecord {
            id: event_id,
            session_id,
            source: instance.id.clone(),
            target: None,
            job_id: None,
            kind: EventKind::InstanceRenamed,
            payload: name,
        });

        Ok(instance)
    }

    pub fn submit_participation_claim(
        &mut self,
        claim: ParticipationClaim,
    ) -> Result<ParticipationClaim, RuntimeError> {
        if !(0.0..=1.0).contains(&claim.score) {
            return Err(RuntimeError::invalid_claim(claim.score));
        }

        let message = self
            .state
            .messages
            .iter()
            .find(|message| message.id == claim.message_id)
            .cloned()
            .ok_or_else(|| RuntimeError::message_not_found(&claim.message_id))?;
        self.ensure_instance_in_session(&message.session_id, &claim.instance_id)?;

        self.state.claims.push(claim.clone());
        let event_id = self.next_id(IdKind::Event);
        self.push_event(EventRecord {
            id: event_id,
            session_id: message.session_id,
            source: claim.instance_id.clone(),
            target: Some(message.id.clone()),
            job_id: None,
            kind: EventKind::ParticipationClaimSubmitted,
            payload: format!("round={} score={:.2}", claim.round, claim.score),
        });

        Ok(claim)
    }

    pub fn resolve_speaking_grant(
        &mut self,
        message_id: &str,
        round: u32,
    ) -> Result<Option<SpeakingGrant>, RuntimeError> {
        let message = self
            .state
            .messages
            .iter()
            .find(|message| message.id == message_id)
            .cloned()
            .ok_or_else(|| RuntimeError::message_not_found(message_id))?;
        let claims = self
            .state
            .claims
            .iter()
            .filter(|claim| claim.message_id == message_id)
            .cloned()
            .collect::<Vec<_>>();
        let grant = select_winner(&claims, &self.nomination_policy, round)
            .map_err(RuntimeError::claim_protocol)?;
        let nomination_index = self
            .state
            .nominations
            .iter()
            .position(|nomination| nomination.message_id == message_id);

        if let Some(grant) = &grant {
            self.state.speaking_grants.push(grant.clone());
            if let Some(index) = nomination_index {
                self.state.nominations[index].status = NominationStatus::Granted;
                self.state.nominations[index].current_round = round;
            }
            let event_id = self.next_id(IdKind::Event);
            self.push_event(EventRecord {
                id: event_id,
                session_id: message.session_id,
                source: "runtime".to_owned(),
                target: Some(grant.instance_id.clone()),
                job_id: None,
                kind: EventKind::SpeakingGranted,
                payload: format!(
                    "message={} round={} score={:.2}",
                    grant.message_id, grant.round, grant.score
                ),
            });
        } else if let Some(index) = nomination_index {
            if let Some(next_round) = self
                .nomination_policy
                .rounds
                .iter()
                .find(|candidate| candidate.round > round)
                .map(|candidate| candidate.round)
            {
                self.state.nominations[index].current_round = next_round;
                let event_id = self.next_id(IdKind::Event);
                self.push_event(EventRecord {
                    id: event_id,
                    session_id: message.session_id,
                    source: "runtime".to_owned(),
                    target: Some(message.id.clone()),
                    job_id: None,
                    kind: EventKind::NominationAdvanced,
                    payload: format!("round={next_round}"),
                });
            } else {
                self.state.nominations[index].status = NominationStatus::Exhausted;
                self.state.nominations[index].current_round = round;
                let event_id = self.next_id(IdKind::Event);
                self.push_event(EventRecord {
                    id: event_id,
                    session_id: message.session_id,
                    source: "runtime".to_owned(),
                    target: Some(message.id.clone()),
                    job_id: None,
                    kind: EventKind::NominationExhausted,
                    payload: format!("round={round}"),
                });
            }
        }

        Ok(grant)
    }

    pub fn mailbox_for_instance(
        &self,
        session_id: &str,
        instance_id: &str,
    ) -> Result<Vec<&MessageRecord>, RuntimeError> {
        self.ensure_instance_in_session(session_id, instance_id)?;

        Ok(self
            .state
            .messages
            .iter()
            .filter(|message| {
                if message.session_id != session_id {
                    return false;
                }

                match &message.route {
                    MessageRoute::Direct { to } => to == instance_id,
                    MessageRoute::Channel { channel_id } => self
                        .instance(instance_id)
                        .map(|instance| instance.channel_ids.iter().any(|id| id == channel_id))
                        .unwrap_or(false),
                    MessageRoute::Broadcast => true,
                }
            })
            .collect())
    }

    pub fn claims_for_message(
        &self,
        message_id: &str,
    ) -> Result<Vec<&ParticipationClaim>, RuntimeError> {
        if !self.state.messages.iter().any(|message| message.id == message_id) {
            return Err(RuntimeError::message_not_found(message_id));
        }

        Ok(self
            .state
            .claims
            .iter()
            .filter(|claim| claim.message_id == message_id)
            .collect())
    }

    pub fn nomination_for_message(
        &self,
        message_id: &str,
    ) -> Result<&NominationRecord, RuntimeError> {
        self.state
            .nominations
            .iter()
            .find(|nomination| nomination.message_id == message_id)
            .ok_or_else(|| RuntimeError::message_not_found(message_id))
    }

    pub fn events_for_instance(
        &self,
        session_id: &str,
        instance_id: &str,
    ) -> Result<Vec<&EventRecord>, RuntimeError> {
        self.ensure_instance_in_session(session_id, instance_id)?;

        Ok(self
            .state
            .events
            .iter()
            .filter(|event| {
                event.session_id == session_id
                    && (event.source == instance_id
                        || event.target.as_deref() == Some(instance_id))
            })
            .collect())
    }

    pub fn plan_run_request(
        &self,
        request: &RunRequest,
    ) -> WorkerPlan {
        let job_kind = classify_run_request(request);
        let child_instance = self.classify_child_instance(request);
        let worker_kind = match job_kind {
            JobKind::Process => WorkerKind::AsyncTask,
            JobKind::Pty => WorkerKind::PtyProcess,
        };

        WorkerPlan {
            job_kind,
            worker_kind,
            child_instance,
        }
    }

    pub fn submit_run_request(
        &mut self,
        instance_id: &str,
        title: impl Into<String>,
        run_request: RunRequest,
    ) -> Result<JobRecord, RuntimeError> {
        let instance_index = self
            .find_instance_index(instance_id)
            .ok_or_else(|| RuntimeError::instance_not_found(instance_id))?;
        let session_id = self.state.instances[instance_index].session_id.clone();
        let plan = self.plan_run_request(&run_request);

        let worker = WorkerRecord {
            id: self.next_id(IdKind::Worker),
            instance_id: instance_id.to_owned(),
            kind: plan.worker_kind,
        };
        let job = JobRecord {
            id: self.next_id(IdKind::Job),
            instance_id: instance_id.to_owned(),
            worker_id: worker.id.clone(),
            kind: plan.job_kind,
            state: JobState::Queued,
            title: title.into(),
            run_request,
        };

        self.state.instances[instance_index]
            .worker_ids
            .push(worker.id.clone());
        self.state.instances[instance_index]
            .job_ids
            .push(job.id.clone());
        self.state.workers.push(worker.clone());
        self.state.jobs.push(job.clone());

        let event_id = self.next_id(IdKind::Event);
        self.push_event(EventRecord {
            id: event_id,
            session_id,
            source: instance_id.to_owned(),
            target: Some(worker.id),
            job_id: Some(job.id.clone()),
            kind: EventKind::JobQueued,
            payload: job.title.clone(),
        });

        Ok(job)
    }

    pub fn update_job_state(
        &mut self,
        job_id: &str,
        next_state: JobState,
    ) -> Result<(), RuntimeError> {
        let job_index = self
            .find_job_index(job_id)
            .ok_or_else(|| RuntimeError::job_not_found(job_id))?;
        let instance_id = self.state.jobs[job_index].instance_id.clone();
        let worker_id = self.state.jobs[job_index].worker_id.clone();
        let job_record_id = self.state.jobs[job_index].id.clone();
        let session_id = self
            .instance(&instance_id)
            .ok_or_else(|| RuntimeError::instance_not_found(&instance_id))?
            .session_id
            .clone();
        self.state.jobs[job_index].state = next_state.clone();

        let event = EventRecord {
            id: self.next_id(IdKind::Event),
            session_id,
            source: worker_id,
            target: Some(instance_id),
            job_id: Some(job_record_id),
            kind: EventKind::JobStateChanged,
            payload: format!("{next_state:?}"),
        };
        self.push_event(event);
        Ok(())
    }

    pub fn append_job_event(
        &mut self,
        job_id: &str,
        kind: EventKind,
        payload: impl Into<String>,
    ) -> Result<EventRecord, RuntimeError> {
        let job = self
            .job(job_id)
            .cloned()
            .ok_or_else(|| RuntimeError::job_not_found(job_id))?;
        let session_id = self
            .instance(&job.instance_id)
            .ok_or_else(|| RuntimeError::instance_not_found(&job.instance_id))?
            .session_id
            .clone();
        let event = EventRecord {
            id: self.next_id(IdKind::Event),
            session_id,
            source: job.worker_id,
            target: Some(job.instance_id),
            job_id: Some(job.id),
            kind,
            payload: payload.into(),
        };
        self.push_event(event.clone());
        Ok(event)
    }

    pub fn promote_job_to_child_instance(
        &mut self,
        job_id: &str,
        child_name: impl Into<String>,
    ) -> Result<InstanceRecord, RuntimeError> {
        let job = self
            .job(job_id)
            .cloned()
            .ok_or_else(|| RuntimeError::job_not_found(job_id))?;
        let parent = self
            .instance(&job.instance_id)
            .cloned()
            .ok_or_else(|| RuntimeError::instance_not_found(&job.instance_id))?;
        let child = self.create_instance(
            &parent.session_id,
            child_name,
            Some(parent.id.clone()),
        )?;

        let event = EventRecord {
            id: self.next_id(IdKind::Event),
            session_id: parent.session_id,
            source: parent.id,
            target: Some(child.id.clone()),
            job_id: Some(job.id),
            kind: EventKind::InstancePromotedFromJob,
            payload: job.title,
        };
        self.push_event(event);
        Ok(child)
    }

    pub fn classify_child_instance(
        &self,
        request: &RunRequest,
    ) -> ChildInstanceDisposition {
        if request.allow_child_instance && classify_run_request(request) == JobKind::Pty {
            ChildInstanceDisposition::PromoteToChildInstance
        } else {
            ChildInstanceDisposition::StayAsJob
        }
    }

    fn ensure_instance_in_session(
        &self,
        session_id: &str,
        instance_id: &str,
    ) -> Result<(), RuntimeError> {
        let instance = self
            .state
            .instances
            .iter()
            .find(|instance| instance.id == instance_id)
            .ok_or_else(|| RuntimeError::instance_not_found(instance_id))?;

        if instance.session_id != session_id {
            return Err(RuntimeError::instance_session_mismatch(
                instance_id,
                session_id,
            ));
        }

        Ok(())
    }

    fn ensure_channel_in_session(
        &self,
        session_id: &str,
        channel_id: &str,
    ) -> Result<(), RuntimeError> {
        let channel = self
            .state
            .channels
            .iter()
            .find(|channel| channel.id == channel_id)
            .ok_or_else(|| RuntimeError::channel_not_found(channel_id))?;

        if channel.session_id != session_id {
            return Err(RuntimeError::channel_session_mismatch(
                channel_id, session_id,
            ));
        }

        Ok(())
    }

    fn ensure_instance_in_channel(
        &self,
        instance_id: &str,
        channel_id: &str,
    ) -> Result<(), RuntimeError> {
        let instance = self
            .state
            .instances
            .iter()
            .find(|instance| instance.id == instance_id)
            .ok_or_else(|| RuntimeError::instance_not_found(instance_id))?;

        if !instance.channel_ids.iter().any(|id| id == channel_id) {
            return Err(RuntimeError::instance_channel_mismatch(
                instance_id,
                channel_id,
            ));
        }

        Ok(())
    }

    fn find_session_index(&self, session_id: &str) -> Option<usize> {
        self.state
            .sessions
            .iter()
            .position(|session| session.id == session_id)
    }

    fn find_instance_index(&self, instance_id: &str) -> Option<usize> {
        self.state
            .instances
            .iter()
            .position(|instance| instance.id == instance_id)
    }

    fn find_channel_index(&self, channel_id: &str) -> Option<usize> {
        self.state
            .channels
            .iter()
            .position(|channel| channel.id == channel_id)
    }

    fn find_job_index(&self, job_id: &str) -> Option<usize> {
        self.state.jobs.iter().position(|job| job.id == job_id)
    }

    fn push_event(&mut self, event: EventRecord) {
        self.state.events.push(event);
    }

    fn next_id(&mut self, kind: IdKind) -> String {
        match kind {
            IdKind::Session => {
                self.next_session += 1;
                format!("session.{:04}", self.next_session)
            }
            IdKind::Channel => {
                self.next_channel += 1;
                format!("channel.{:04}", self.next_channel)
            }
            IdKind::Instance => {
                self.next_instance += 1;
                format!("instance.{:04}", self.next_instance)
            }
            IdKind::Worker => {
                self.next_worker += 1;
                format!("worker.{:04}", self.next_worker)
            }
            IdKind::Job => {
                self.next_job += 1;
                format!("job.{:04}", self.next_job)
            }
            IdKind::Message => {
                self.next_message += 1;
                format!("message.{:04}", self.next_message)
            }
            IdKind::Event => {
                self.next_event += 1;
                format!("event.{:04}", self.next_event)
            }
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum IdKind {
    Session,
    Channel,
    Instance,
    Worker,
    Job,
    Message,
    Event,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum RuntimeError {
    #[error("session not found: {0}")]
    SessionNotFound(String),
    #[error("instance not found: {0}")]
    InstanceNotFound(String),
    #[error("instance {instance_id} is not in session {session_id}")]
    InstanceSessionMismatch {
        instance_id: String,
        session_id: String,
    },
    #[error("channel not found: {0}")]
    ChannelNotFound(String),
    #[error("channel {channel_id} is not in session {session_id}")]
    ChannelSessionMismatch {
        channel_id: String,
        session_id: String,
    },
    #[error("instance {instance_id} is not in channel {channel_id}")]
    InstanceChannelMismatch {
        instance_id: String,
        channel_id: String,
    },
    #[error("job not found: {0}")]
    JobNotFound(String),
    #[error("message not found: {0}")]
    MessageNotFound(String),
    #[error("invalid claim score: {0}")]
    InvalidClaimScore(String),
    #[error("claim protocol error: {0}")]
    ClaimProtocol(String),
}

impl RuntimeError {
    fn session_not_found(session_id: &str) -> Self {
        Self::SessionNotFound(session_id.to_owned())
    }

    fn instance_not_found(instance_id: &str) -> Self {
        Self::InstanceNotFound(instance_id.to_owned())
    }

    fn instance_session_mismatch(instance_id: &str, session_id: &str) -> Self {
        Self::InstanceSessionMismatch {
            instance_id: instance_id.to_owned(),
            session_id: session_id.to_owned(),
        }
    }

    fn channel_not_found(channel_id: &str) -> Self {
        Self::ChannelNotFound(channel_id.to_owned())
    }

    fn channel_session_mismatch(channel_id: &str, session_id: &str) -> Self {
        Self::ChannelSessionMismatch {
            channel_id: channel_id.to_owned(),
            session_id: session_id.to_owned(),
        }
    }

    fn instance_channel_mismatch(instance_id: &str, channel_id: &str) -> Self {
        Self::InstanceChannelMismatch {
            instance_id: instance_id.to_owned(),
            channel_id: channel_id.to_owned(),
        }
    }

    fn job_not_found(job_id: &str) -> Self {
        Self::JobNotFound(job_id.to_owned())
    }

    fn message_not_found(message_id: &str) -> Self {
        Self::MessageNotFound(message_id.to_owned())
    }

    fn invalid_claim(score: f32) -> Self {
        Self::InvalidClaimScore(format!("{score:.4}"))
    }

    fn claim_protocol(error: ClaimError) -> Self {
        Self::ClaimProtocol(error.to_string())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionRecord {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub namespace: RuntimeNamespace,
    pub instance_ids: Vec<String>,
    pub channel_ids: Vec<String>,
}

impl RecordId for SessionRecord {
    fn id(&self) -> &str {
        &self.id
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChannelRecord {
    pub id: String,
    pub session_id: String,
    #[serde(default)]
    pub namespace: RuntimeNamespace,
    pub name: String,
    pub member_instance_ids: Vec<String>,
}

impl RecordId for ChannelRecord {
    fn id(&self) -> &str {
        &self.id
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InstanceRecord {
    pub id: String,
    pub session_id: String,
    #[serde(default)]
    pub namespace: RuntimeNamespace,
    pub name: String,
    pub parent_instance_id: Option<String>,
    pub child_instance_ids: Vec<String>,
    pub channel_ids: Vec<String>,
    pub job_ids: Vec<String>,
    pub worker_ids: Vec<String>,
}

impl RecordId for InstanceRecord {
    fn id(&self) -> &str {
        &self.id
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkerRecord {
    pub id: String,
    pub instance_id: String,
    pub kind: WorkerKind,
}

impl RecordId for WorkerRecord {
    fn id(&self) -> &str {
        &self.id
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkerKind {
    AsyncTask,
    Thread,
    Process,
    PtyProcess,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct JobRecord {
    pub id: String,
    pub instance_id: String,
    pub worker_id: String,
    pub kind: JobKind,
    pub state: JobState,
    pub title: String,
    pub run_request: RunRequest,
}

impl RecordId for JobRecord {
    fn id(&self) -> &str {
        &self.id
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum JobKind {
    Process,
    Pty,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    Queued,
    Running,
    WaitingInput,
    Succeeded,
    Failed,
    Killed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    SessionCreated,
    ChannelCreated,
    ChannelJoined,
    ChannelLeft,
    InstanceCreated,
    InstanceRenamed,
    MessagePosted,
    NominationOpened,
    NominationAdvanced,
    NominationExhausted,
    ParticipationClaimSubmitted,
    SpeakingGranted,
    JobQueued,
    JobStateChanged,
    JobStdout,
    JobStderr,
    InstancePromotedFromJob,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunRequest {
    pub program: String,
    pub args: Vec<String>,
    pub cwd: Option<String>,
    pub run_mode: RunMode,
    pub interactive: bool,
    pub allow_child_instance: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RunMode {
    Process,
    Pty,
    Auto,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ChildInstanceDisposition {
    StayAsJob,
    PromoteToChildInstance,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkerPlan {
    pub job_kind: JobKind,
    pub worker_kind: WorkerKind,
    pub child_instance: ChildInstanceDisposition,
}

pub fn classify_run_request(request: &RunRequest) -> JobKind {
    match request.run_mode {
        RunMode::Process => JobKind::Process,
        RunMode::Pty => JobKind::Pty,
        RunMode::Auto => auto_job_kind(request),
    }
}

fn auto_job_kind(request: &RunRequest) -> JobKind {
    let program = request.program.to_ascii_lowercase();
    let requires_terminal = request.interactive
        || matches!(
            program.as_str(),
            "bash" | "sh" | "zsh" | "fish" | "pwsh" | "powershell" | "cmd" | "python" | "node"
        );

    if requires_terminal {
        JobKind::Pty
    } else {
        JobKind::Process
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MessageRecord {
    pub id: String,
    pub session_id: String,
    pub from: String,
    pub route: MessageRoute,
    pub kind: MessageKind,
    pub body: String,
    pub reply_to: Option<String>,
}

impl RecordId for MessageRecord {
    fn id(&self) -> &str {
        &self.id
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct NominationRecord {
    pub message_id: String,
    pub session_id: String,
    pub route: MessageRoute,
    pub current_round: u32,
    pub status: NominationStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum NominationStatus {
    Open,
    Granted,
    Exhausted,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MessageKind {
    Chat,
    Command,
    Control,
    System,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "route", rename_all = "snake_case")]
pub enum MessageRoute {
    Direct { to: String },
    Broadcast,
    Channel { channel_id: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EventRecord {
    pub id: String,
    pub session_id: String,
    pub source: String,
    pub target: Option<String>,
    pub job_id: Option<String>,
    pub kind: EventKind,
    pub payload: String,
}

impl RecordId for EventRecord {
    fn id(&self) -> &str {
        &self.id
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn broadcast_reaches_all_instances_in_session() {
        let mut runtime = RuntimeSupervisor::new();
        let session = runtime.create_session("demo");
        let alice = runtime
            .create_instance(&session.id, "alice", None)
            .expect("alice should be created");
        let bob = runtime
            .create_instance(&session.id, "bob", None)
            .expect("bob should be created");

        runtime
            .post_message(
                &session.id,
                &alice.id,
                MessageRoute::Broadcast,
                MessageKind::Chat,
                "hello all",
                None,
            )
            .expect("broadcast should succeed");

        let alice_mailbox = runtime
            .mailbox_for_instance(&session.id, &alice.id)
            .expect("alice mailbox should load");
        let bob_mailbox = runtime
            .mailbox_for_instance(&session.id, &bob.id)
            .expect("bob mailbox should load");

        assert_eq!(alice_mailbox.len(), 1);
        assert_eq!(bob_mailbox.len(), 1);
        assert_eq!(alice_mailbox[0].body, "hello all");
        assert_eq!(bob_mailbox[0].body, "hello all");
    }

    #[test]
    fn channel_message_only_reaches_subscribed_instances() {
        let mut runtime = RuntimeSupervisor::new();
        let session = runtime.create_session("demo");
        let alice = runtime
            .create_instance(&session.id, "alice", None)
            .expect("alice should be created");
        let bob = runtime
            .create_instance(&session.id, "bob", None)
            .expect("bob should be created");
        let carol = runtime
            .create_instance(&session.id, "carol", None)
            .expect("carol should be created");
        let channel = runtime
            .create_channel(&session.id, "planning")
            .expect("channel should be created");

        runtime
            .join_channel(&alice.id, &channel.id)
            .expect("alice should join");
        runtime
            .join_channel(&bob.id, &channel.id)
            .expect("bob should join");

        runtime
            .post_message(
                &session.id,
                &alice.id,
                MessageRoute::Channel {
                    channel_id: channel.id.clone(),
                },
                MessageKind::Chat,
                "plan update",
                None,
            )
            .expect("channel message should succeed");

        let alice_mailbox = runtime
            .mailbox_for_instance(&session.id, &alice.id)
            .expect("alice mailbox should load");
        let bob_mailbox = runtime
            .mailbox_for_instance(&session.id, &bob.id)
            .expect("bob mailbox should load");
        let carol_mailbox = runtime
            .mailbox_for_instance(&session.id, &carol.id)
            .expect("carol mailbox should load");

        assert_eq!(alice_mailbox.len(), 1);
        assert_eq!(bob_mailbox.len(), 1);
        assert!(carol_mailbox.is_empty());
        assert_eq!(
            alice_mailbox[0].route,
            MessageRoute::Channel {
                channel_id: channel.id.clone()
            }
        );
        assert_eq!(
            bob_mailbox[0].route,
            MessageRoute::Channel {
                channel_id: channel.id.clone()
            }
        );
    }

    #[test]
    fn runtime_namespace_propagates_from_session_to_instances_and_channels() {
        let mut runtime = RuntimeSupervisor::new();
        let session = runtime.create_session_in_namespace(
            "demo",
            RuntimeNamespace::new("tenant-a", "user-a"),
        );
        let instance = runtime
            .create_instance(&session.id, "alice", None)
            .expect("instance should be created");
        let channel = runtime
            .create_channel(&session.id, "planning")
            .expect("channel should be created");

        assert_eq!(session.namespace.tenant_id, "tenant-a");
        assert_eq!(session.namespace.user_id, "user-a");
        assert_eq!(instance.namespace, session.namespace);
        assert_eq!(channel.namespace, session.namespace);
    }

}
