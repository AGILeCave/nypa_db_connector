use std::{
    fmt,
    sync::{
        Mutex,
        mpsc::{self, Receiver, Sender, TryRecvError},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use bevy::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

const DEFAULT_CNC_ENDPOINT: &str = "http://127.0.0.1:48080";
const DEFAULT_CONTROL_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone, Debug)]
pub struct NypaDbControlPlugin {
    endpoint: String,
}

impl Default for NypaDbControlPlugin {
    fn default() -> Self {
        Self::new(DEFAULT_CNC_ENDPOINT)
    }
}

impl NypaDbControlPlugin {
    pub fn new(endpoint: impl Into<String>) -> Self {
        Self {
            endpoint: endpoint.into(),
        }
    }
}

impl Plugin for NypaDbControlPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(NypaDbControl::connect(self.endpoint.clone()));
        app.init_resource::<NypaDbVariables>();
        app.add_systems(Update, drain_nypa_control);
    }
}

#[derive(Resource)]
pub struct NypaDbControl {
    tx: Sender<ControlRequest>,
    rx: Mutex<Receiver<ControlResponse>>,
    worker: Mutex<Option<JoinHandle<()>>>,
}

impl NypaDbControl {
    pub fn connect(endpoint: impl Into<String>) -> Self {
        let endpoint = endpoint.into();
        let (request_tx, request_rx) = mpsc::channel();
        let (response_tx, response_rx) = mpsc::channel();
        let worker = thread::spawn(move || control_worker(endpoint, request_rx, response_tx));

        Self {
            tx: request_tx,
            rx: Mutex::new(response_rx),
            worker: Mutex::new(Some(worker)),
        }
    }

    pub fn request_variables(&self, stream_id: usize) -> Result<(), NypaDbControlQueueError> {
        self.send(NypaDbControlOperation::GetVariables { stream_id })
    }

    pub fn set_variable(
        &self,
        stream_id: usize,
        name: impl Into<String>,
        value: f32,
    ) -> Result<(), NypaDbControlQueueError> {
        self.send(NypaDbControlOperation::SetVariable {
            stream_id,
            name: name.into(),
            value,
        })
    }

    pub fn reset_variables(&self, stream_id: usize) -> Result<(), NypaDbControlQueueError> {
        self.send(NypaDbControlOperation::ResetVariables { stream_id })
    }

    fn send(&self, operation: NypaDbControlOperation) -> Result<(), NypaDbControlQueueError> {
        self.tx
            .send(ControlRequest::Command(operation))
            .map_err(|_| NypaDbControlQueueError)
    }

    fn drain(&self, mut f: impl FnMut(ControlResponse)) {
        let rx = self
            .rx
            .lock()
            .expect("NYPA DB control response lock poisoned");

        loop {
            match rx.try_recv() {
                Ok(response) => f(response),
                Err(TryRecvError::Empty) => return,
                Err(TryRecvError::Disconnected) => return,
            }
        }
    }
}

impl Drop for NypaDbControl {
    fn drop(&mut self) {
        let _ = self.tx.send(ControlRequest::Stop);

        if let Some(worker) = self
            .worker
            .lock()
            .expect("NYPA DB control worker lock poisoned")
            .take()
        {
            let _ = worker.join();
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct NypaDbControlQueueError;

impl fmt::Display for NypaDbControlQueueError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("NYPA DB control worker is closed")
    }
}

impl std::error::Error for NypaDbControlQueueError {}

#[derive(Clone, Debug, PartialEq)]
pub enum NypaDbControlOperation {
    GetVariables {
        stream_id: usize,
    },
    SetVariable {
        stream_id: usize,
        name: String,
        value: f32,
    },
    ResetVariables {
        stream_id: usize,
    },
}

impl NypaDbControlOperation {
    pub fn stream_id(&self) -> usize {
        match self {
            Self::GetVariables { stream_id }
            | Self::SetVariable { stream_id, .. }
            | Self::ResetVariables { stream_id } => *stream_id,
        }
    }
}

#[derive(Resource, Default, Clone, Debug)]
pub struct NypaDbVariables {
    pub streams: Vec<NypaDbVariableStream>,
}

impl NypaDbVariables {
    pub fn stream(&self, stream_id: usize) -> Option<&NypaDbVariableStream> {
        self.streams
            .iter()
            .find(|stream| stream.stream_id == stream_id)
    }

    pub fn variable(&self, stream_id: usize, internal_name: &str) -> Option<&NypaDbVariable> {
        self.stream(stream_id)?
            .variables
            .iter()
            .find(|variable| variable.internal_name == internal_name)
    }

    fn upsert_stream(&mut self, stream: NypaDbVariableStream) {
        match self
            .streams
            .iter_mut()
            .find(|current| current.stream_id == stream.stream_id)
        {
            Some(current) => *current = stream,
            None => {
                self.streams.push(stream);
                self.streams.sort_by_key(|stream| stream.stream_id);
            }
        }
    }

    fn apply_update(&mut self, update: &NypaDbVariableSet) {
        let Some(variable) = self
            .streams
            .iter_mut()
            .find(|stream| stream.stream_id == update.stream_id)
            .and_then(|stream| {
                stream
                    .variables
                    .iter_mut()
                    .find(|variable| variable.internal_name == update.name)
            })
        else {
            return;
        };

        variable.value = update.value;
    }

    fn apply_reset(&mut self, reset: &NypaDbVariableReset) {
        if reset.skipped {
            return;
        }

        let Some(stream) = self
            .streams
            .iter_mut()
            .find(|stream| stream.stream_id == reset.stream_id)
        else {
            return;
        };

        for variable in &mut stream.variables {
            variable.value = variable.initial_value;
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct NypaDbVariableStream {
    pub stream_id: usize,
    pub strategy: NypaDbVariableStrategy,
    pub variables: Vec<NypaDbVariable>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NypaDbVariableStrategy {
    JsonRpc,
    StateVector,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct NypaDbVariable {
    pub name: String,
    pub internal_name: String,
    pub description: Option<String>,
    pub initial_value: f32,
    pub value: f32,
    pub min: Option<f32>,
    pub max: Option<f32>,
    pub semantic: NypaDbVariableSemantic,
    pub index: Option<usize>,
    pub source: Option<usize>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NypaDbVariableSemantic {
    Bool,
    Real,
}

#[derive(Clone, Debug, Event)]
pub struct NypaDbVariablesChanged {
    pub stream_id: usize,
}

#[derive(Clone, Debug, Event)]
pub struct NypaDbVariableSet {
    pub stream_id: usize,
    pub name: String,
    pub value: f32,
}

#[derive(Clone, Debug, Event)]
pub struct NypaDbVariableReset {
    pub stream_id: usize,
    pub strategy: NypaDbVariableStrategy,
    pub reset_count: usize,
    pub skipped: bool,
    pub message: String,
}

#[derive(Clone, Debug, Event)]
pub struct NypaDbControlError {
    pub stream_id: usize,
    pub operation: NypaDbControlOperation,
    pub message: String,
}

enum ControlRequest {
    Command(NypaDbControlOperation),
    Stop,
}

enum ControlResponse {
    Variables(NypaDbVariableStream),
    VariableSet(NypaDbVariableSet),
    VariableReset(NypaDbVariableReset),
    Error(NypaDbControlError),
}

fn drain_nypa_control(
    control: Res<NypaDbControl>,
    mut variables: ResMut<NypaDbVariables>,
    mut commands: Commands,
) {
    control.drain(|response| match response {
        ControlResponse::Variables(stream) => {
            let stream_id = stream.stream_id;
            variables.upsert_stream(stream);
            commands.trigger(NypaDbVariablesChanged { stream_id });
        }
        ControlResponse::VariableSet(update) => {
            variables.apply_update(&update);
            commands.trigger(NypaDbVariablesChanged {
                stream_id: update.stream_id,
            });
            commands.trigger(update);
        }
        ControlResponse::VariableReset(reset) => {
            variables.apply_reset(&reset);
            commands.trigger(NypaDbVariablesChanged {
                stream_id: reset.stream_id,
            });
            commands.trigger(reset);
        }
        ControlResponse::Error(error) => {
            commands.trigger(error);
        }
    });
}

fn control_worker(
    endpoint: String,
    request_rx: Receiver<ControlRequest>,
    response_tx: Sender<ControlResponse>,
) {
    let mut id = 1_u64;
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(DEFAULT_CONTROL_TIMEOUT))
        .build()
        .into();

    while let Ok(request) = request_rx.recv() {
        let operation = match request {
            ControlRequest::Command(operation) => operation,
            ControlRequest::Stop => return,
        };

        let response = match send_operation(&agent, &endpoint, id, &operation) {
            Ok(response) => response,
            Err(message) => ControlResponse::Error(NypaDbControlError {
                stream_id: operation.stream_id(),
                operation,
                message,
            }),
        };

        id = id.saturating_add(1);

        if response_tx.send(response).is_err() {
            return;
        }
    }
}

fn send_operation(
    agent: &ureq::Agent,
    endpoint: &str,
    id: u64,
    operation: &NypaDbControlOperation,
) -> Result<ControlResponse, String> {
    let (method, params) = operation_rpc(operation);
    let request = JsonRpcRequest {
        jsonrpc: "2.0",
        id,
        method,
        params,
    };

    let response = agent
        .post(endpoint)
        .send_json(request)
        .map_err(|err| err.to_string())?
        .body_mut()
        .read_json::<JsonRpcResponse>()
        .map_err(|err| err.to_string())?;

    if let Some(error) = response.error {
        return Err(error.to_string());
    }

    let result = response
        .result
        .ok_or_else(|| "JSON-RPC response did not include a result".to_string())?;

    match operation {
        NypaDbControlOperation::GetVariables { .. } => {
            let stream = serde_json::from_value::<NypaDbVariableStream>(result)
                .map_err(|err| err.to_string())?;
            Ok(ControlResponse::Variables(stream))
        }
        NypaDbControlOperation::SetVariable { .. } => {
            let update = serde_json::from_value::<VariableUpdateResult>(result)
                .map_err(|err| err.to_string())?;
            Ok(ControlResponse::VariableSet(NypaDbVariableSet {
                stream_id: update.stream_id,
                name: update.name,
                value: update.value,
            }))
        }
        NypaDbControlOperation::ResetVariables { .. } => {
            let reset = serde_json::from_value::<VariableResetResult>(result)
                .map_err(|err| err.to_string())?;
            Ok(ControlResponse::VariableReset(NypaDbVariableReset {
                stream_id: reset.stream_id,
                strategy: reset.strategy,
                reset_count: reset.reset_count,
                skipped: reset.skipped,
                message: reset.message,
            }))
        }
    }
}

fn operation_rpc(operation: &NypaDbControlOperation) -> (&'static str, Value) {
    match operation {
        NypaDbControlOperation::GetVariables { stream_id } => {
            ("get_variables", json!({ "stream_id": stream_id }))
        }
        NypaDbControlOperation::SetVariable {
            stream_id,
            name,
            value,
        } => (
            "set_variable",
            json!({
                "stream_id": stream_id,
                "name": name,
                "value": value,
            }),
        ),
        NypaDbControlOperation::ResetVariables { stream_id } => {
            ("reset_variables", json!({ "stream_id": stream_id }))
        }
    }
}

#[derive(Serialize)]
struct JsonRpcRequest {
    jsonrpc: &'static str,
    id: u64,
    method: &'static str,
    params: Value,
}

#[derive(Deserialize)]
struct JsonRpcResponse {
    result: Option<Value>,
    error: Option<JsonRpcError>,
}

#[derive(Deserialize)]
struct JsonRpcError {
    code: i64,
    message: String,
    data: Option<Value>,
}

impl fmt::Display for JsonRpcError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.data {
            Some(data) => write!(f, "JSON-RPC error {}: {} ({data})", self.code, self.message),
            None => write!(f, "JSON-RPC error {}: {}", self.code, self.message),
        }
    }
}

#[derive(Deserialize)]
struct VariableUpdateResult {
    stream_id: usize,
    name: String,
    value: f32,
}

#[derive(Deserialize)]
struct VariableResetResult {
    stream_id: usize,
    strategy: NypaDbVariableStrategy,
    reset_count: usize,
    skipped: bool,
    message: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn variables_upsert_keeps_streams_sorted() {
        let mut variables = NypaDbVariables::default();

        variables.upsert_stream(test_stream(2, 1.0));
        variables.upsert_stream(test_stream(0, 2.0));
        variables.upsert_stream(test_stream(2, 3.0));

        assert_eq!(variables.streams.len(), 2);
        assert_eq!(variables.streams[0].stream_id, 0);
        assert_eq!(variables.streams[1].stream_id, 2);
        assert_eq!(variables.streams[1].variables[0].value, 3.0);
    }

    #[test]
    fn variable_update_changes_reflected_value() {
        let mut variables = NypaDbVariables::default();
        variables.upsert_stream(test_stream(0, 1.0));

        variables.apply_update(&NypaDbVariableSet {
            stream_id: 0,
            name: "gain".to_string(),
            value: 2.5,
        });

        assert_eq!(variables.variable(0, "gain").unwrap().value, 2.5);
    }

    #[test]
    fn state_vector_reset_restores_initial_reflected_values() {
        let mut variables = NypaDbVariables::default();
        variables.upsert_stream(test_stream(0, 2.5));

        variables.apply_reset(&NypaDbVariableReset {
            stream_id: 0,
            strategy: NypaDbVariableStrategy::StateVector,
            reset_count: 1,
            skipped: false,
            message: "reset 1 state_vector variable(s)".to_string(),
        });

        assert_eq!(variables.variable(0, "gain").unwrap().value, 1.0);
    }

    #[test]
    fn parses_variable_snapshot_result() {
        let result = json!({
            "stream_id": 0,
            "strategy": "state_vector",
            "variables": [{
                "name": "Breaker",
                "internal_name": "breaker",
                "description": "main breaker",
                "initial_value": 0.0,
                "value": 1.0,
                "min": 0.0,
                "max": 1.0,
                "semantic": "bool",
                "index": 3,
                "source": 1
            }]
        });

        let stream: NypaDbVariableStream = serde_json::from_value(result).unwrap();

        assert_eq!(stream.strategy, NypaDbVariableStrategy::StateVector);
        assert_eq!(stream.variables[0].semantic, NypaDbVariableSemantic::Bool);
        assert_eq!(stream.variables[0].source, Some(1));
    }

    fn test_stream(stream_id: usize, value: f32) -> NypaDbVariableStream {
        NypaDbVariableStream {
            stream_id,
            strategy: NypaDbVariableStrategy::JsonRpc,
            variables: vec![NypaDbVariable {
                name: "Gain".to_string(),
                internal_name: "gain".to_string(),
                description: None,
                initial_value: 1.0,
                value,
                min: Some(0.0),
                max: Some(10.0),
                semantic: NypaDbVariableSemantic::Real,
                index: None,
                source: None,
            }],
        }
    }
}
