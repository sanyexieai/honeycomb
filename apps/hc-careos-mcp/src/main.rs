use std::collections::HashMap;
use std::convert::Infallible;
use std::io::{self, BufRead, BufReader, Write};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};
use std::{env, net::SocketAddr};

use anyhow::{Context, Result, bail};
use axum::{
    Json, Router,
    extract::{Query, State},
    http::StatusCode,
    response::{
        IntoResponse,
        sse::{Event, KeepAlive, Sse},
    },
    routing::{get, post},
};
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

#[derive(Debug, Clone, Copy)]
struct ServiceDef {
    id: &'static str,
    slug: &'static str,
    port: u16,
    description: &'static str,
    tools: &'static [&'static str],
}

const SERVICES: &[ServiceDef] = &[
    service(
        "careos-activities",
        "activities",
        3001,
        "社区活动管理",
        &[
            "list_activities",
            "create_activity",
            "signup_activity",
            "cancel_signup",
        ],
    ),
    service(
        "careos-watch-health",
        "watch-health",
        3002,
        "手表健康数据查询",
        &[
            "get_latest_health",
            "get_heart_rate",
            "get_blood_pressure",
            "get_steps",
            "get_temperature",
            "get_health_history",
            "get_health_summary",
            "get_health_trend",
        ],
    ),
    service(
        "careos-emergency",
        "emergency",
        3003,
        "紧急通知服务",
        &[
            "send_sos_alert",
            "send_bulk_notification",
            "trigger_sop",
            "get_alerts",
            "handle_alert",
            "send_sms",
            "make_phone_call",
        ],
    ),
    service(
        "careos-phone-sms",
        "phone-sms",
        3004,
        "电话短信服务",
        &["send_sms", "call_phone"],
    ),
    service(
        "careos-medication-reminder",
        "medication-reminder",
        3011,
        "用药提醒服务",
        &[
            "list_medications",
            "add_medication",
            "record_medication_taken",
            "get_reminders",
            "set_reminder",
            "get_today_schedule",
            "get_compliance_rate",
        ],
    ),
    service(
        "careos-food-delivery",
        "food-delivery",
        3012,
        "外卖点餐服务",
        &[
            "list_restaurants",
            "get_restaurant_menu",
            "create_order",
            "get_orders",
            "estimate_delivery",
            "get_addresses",
        ],
    ),
    service(
        "careos-ride-hailing",
        "ride-hailing",
        3013,
        "打车叫车服务",
        &[
            "list_car_types",
            "estimate_price",
            "call_car",
            "get_orders",
            "get_recent_places",
            "cancel_order",
            "rate_driver",
        ],
    ),
    service(
        "careos-consultation",
        "consultation",
        3014,
        "在线问诊服务",
        &[
            "list_departments",
            "list_doctors",
            "start_consultation",
            "get_consultations",
            "get_prescription",
            "get_health_tips",
            "symptom_check",
        ],
    ),
    service(
        "careos-housekeeping",
        "housekeeping",
        3015,
        "家政服务",
        &[
            "list_services",
            "list_workers",
            "book_service",
            "get_orders",
            "cancel_order",
            "rate_service",
            "get_price_estimate",
        ],
    ),
    service(
        "careos-health-supplement",
        "health-supplement",
        3016,
        "保健品推荐",
        &[
            "list_products",
            "get_recommendations",
            "check_interactions",
            "get_health_profile",
            "save_health_profile",
            "search_by_symptom",
        ],
    ),
    service(
        "careos-image-diagnosis",
        "image-diagnosis",
        3017,
        "图片远程诊断",
        &[
            "list_diagnosis_types",
            "submit_diagnosis",
            "get_diagnosis_result",
            "get_diagnosis_report",
            "get_upload_guide",
        ],
    ),
    service(
        "careos-music",
        "music",
        3018,
        "在线听歌",
        &[
            "list_songs",
            "play_song",
            "get_favorites",
            "list_playlists",
            "get_recommendations",
            "search_songs",
        ],
    ),
    service(
        "careos-finance",
        "finance",
        3019,
        "理财咨询",
        &[
            "list_products",
            "get_recommendations",
            "calculate_returns",
            "get_portfolio",
            "get_financial_tips",
            "risk_assessment",
            "retirement_planning",
        ],
    ),
    service(
        "careos-travel",
        "travel",
        3020,
        "旅游规划",
        &[
            "list_tours",
            "book_tour",
            "get_bookings",
            "get_travel_checklist",
            "get_travel_insurance",
            "estimate_travel_cost",
        ],
    ),
    service(
        "careos-audiobook",
        "audiobook",
        3021,
        "在线听书",
        &[
            "list_books",
            "play_book",
            "get_favorites",
            "list_categories",
            "get_recommendations",
            "get_chapters",
        ],
    ),
];

const fn service(
    id: &'static str,
    slug: &'static str,
    port: u16,
    description: &'static str,
    tools: &'static [&'static str],
) -> ServiceDef {
    ServiceDef {
        id,
        slug,
        port,
        description,
        tools,
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = env::args().skip(1).collect::<Vec<_>>();
    if args.first().is_some_and(|arg| arg == "--http") {
        let options = parse_http_options(&args)?;
        run_http_server(options).await
    } else {
        let service_id = args.first().context("usage: hc-careos-mcp <service>")?;
        let service = find_service(service_id)?;
        run_stdio_server(service)
    }
}

#[derive(Debug, Clone)]
struct HttpOptions {
    service: ServiceDef,
    port: u16,
}

#[derive(Clone)]
struct HttpState {
    service: ServiceDef,
    sessions: Arc<Mutex<HashMap<String, mpsc::Sender<Result<Event, Infallible>>>>>,
}

fn parse_http_options(args: &[String]) -> Result<HttpOptions> {
    let mut service = None;
    let mut port = None;
    let mut index = 1usize;
    while index < args.len() {
        match args[index].as_str() {
            "--service" => {
                service = Some(find_service(
                    args.get(index + 1).context("missing value for --service")?,
                )?);
                index += 2;
            }
            "--port" => {
                port = Some(
                    args.get(index + 1)
                        .context("missing value for --port")?
                        .parse()
                        .context("invalid --port")?,
                );
                index += 2;
            }
            other => bail!("unknown http option: {other}"),
        }
    }
    let service = service.context("missing --service")?;
    Ok(HttpOptions {
        port: port.unwrap_or(service.port),
        service,
    })
}

async fn run_http_server(options: HttpOptions) -> Result<()> {
    let service = options.service;
    let state = HttpState {
        service,
        sessions: Arc::new(Mutex::new(HashMap::new())),
    };
    let app = Router::new()
        .route("/mcp", post(move |body| handle_http_mcp(service, body)))
        .route("/sse", get(handle_sse_connect))
        .route("/messages", post(handle_sse_message))
        .with_state(state);
    let addr = SocketAddr::from(([127, 0, 0, 1], options.port));
    eprintln!(
        "{} listening on http://{addr}/sse and http://{addr}/mcp",
        service.id
    );
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("failed to bind {addr}"))?;
    axum::serve(listener, app)
        .await
        .context("http server failed")
}

async fn handle_http_mcp(service: ServiceDef, Json(message): Json<Value>) -> Json<Value> {
    Json(handle_mcp_message(service, message))
}

async fn handle_sse_connect(
    State(state): State<HttpState>,
) -> Sse<ReceiverStream<Result<Event, Infallible>>> {
    let session_id = new_session_id();
    let (tx, rx) = mpsc::channel::<Result<Event, Infallible>>(32);
    if let Ok(mut sessions) = state.sessions.lock() {
        sessions.insert(session_id.clone(), tx.clone());
    }
    let endpoint = format!("/messages?sessionId={session_id}");
    let _ = tx
        .send(Ok(Event::default().event("endpoint").data(endpoint)))
        .await;
    Sse::new(ReceiverStream::new(rx)).keep_alive(KeepAlive::default())
}

async fn handle_sse_message(
    State(state): State<HttpState>,
    Query(query): Query<HashMap<String, String>>,
    Json(message): Json<Value>,
) -> impl IntoResponse {
    let Some(session_id) = query.get("sessionId") else {
        return (StatusCode::BAD_REQUEST, "missing sessionId").into_response();
    };
    let sender = state
        .sessions
        .lock()
        .ok()
        .and_then(|sessions| sessions.get(session_id).cloned());
    let Some(sender) = sender else {
        return (StatusCode::NOT_FOUND, "unknown sessionId").into_response();
    };
    if message.get("id").is_some() {
        let response = handle_mcp_message(state.service, message);
        let _ = sender
            .send(Ok(Event::default()
                .event("message")
                .data(response.to_string())))
            .await;
    }
    StatusCode::ACCEPTED.into_response()
}

fn new_session_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    format!("careos-{nanos}")
}

fn run_stdio_server(service: ServiceDef) -> Result<()> {
    let stdin = io::stdin();
    let mut reader = BufReader::new(stdin.lock());
    let mut stdout = io::stdout().lock();
    loop {
        let message = match read_message(&mut reader) {
            Ok(message) => message,
            Err(error) if is_clean_shutdown(&error) => return Ok(()),
            Err(error) => return Err(error),
        };
        if message.get("id").is_none() {
            continue;
        }
        write_message(&mut stdout, &handle_mcp_message(service, message))?;
    }
}

fn handle_mcp_message(service: ServiceDef, message: Value) -> Value {
    let id = message.get("id").cloned().unwrap_or(Value::Null);
    let Some(method) = message.get("method").and_then(Value::as_str) else {
        return json!({"jsonrpc": "2.0", "id": id, "result": Value::Null});
    };
    let result = match method {
        "initialize" => Ok(json!({
            "protocolVersion": "2025-06-18",
            "capabilities": { "tools": {} },
            "serverInfo": {
                "name": service.id,
                "version": env!("CARGO_PKG_VERSION")
            }
        })),
        "tools/list" => Ok(json!({
            "tools": service.tools.iter().map(|tool| render_tool(service, tool)).collect::<Vec<_>>()
        })),
        "tools/call" => {
            let params = message.get("params").cloned().unwrap_or_else(|| json!({}));
            call_tool(service, &params)
        }
        _ => Err(anyhow::anyhow!("unsupported method: {method}")),
    };
    match result {
        Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
        Err(error) => json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": { "code": -32000, "message": error.to_string() }
        }),
    }
}

fn read_message(reader: &mut impl BufRead) -> Result<Value> {
    let mut content_length = None;
    loop {
        let mut line = String::new();
        let read = reader
            .read_line(&mut line)
            .context("failed to read mcp header")?;
        if read == 0 {
            bail!("mcp stdin closed");
        }
        let header = line.trim_end_matches(['\r', '\n']);
        if header.is_empty() {
            break;
        }
        if let Some(value) = header.strip_prefix("Content-Length:") {
            content_length = Some(
                value
                    .trim()
                    .parse::<usize>()
                    .context("invalid Content-Length")?,
            );
        }
    }
    let content_length = content_length.context("missing Content-Length")?;
    let mut body = vec![0; content_length];
    reader
        .read_exact(&mut body)
        .context("failed to read mcp body")?;
    serde_json::from_slice(&body).context("failed to parse mcp body")
}

fn write_message(stdout: &mut impl Write, message: &Value) -> Result<()> {
    let body = serde_json::to_vec(message).context("failed to serialize mcp response")?;
    write!(stdout, "Content-Length: {}\r\n\r\n", body.len())
        .context("failed to write mcp response header")?;
    stdout
        .write_all(&body)
        .context("failed to write mcp response body")?;
    stdout.flush().context("failed to flush mcp response")
}

fn is_clean_shutdown(error: &anyhow::Error) -> bool {
    error.to_string().contains("mcp stdin closed")
}

fn render_tool(service: ServiceDef, tool_name: &str) -> Value {
    json!({
        "name": tool_name,
        "description": tool_description(service, tool_name),
        "inputSchema": {
            "type": "object",
            "additionalProperties": true,
            "properties": {
                "query": { "type": "string" },
                "id": { "type": "string" },
                "user_id": { "type": "string" },
                "confirm": { "type": "boolean" }
            }
        }
    })
}

fn tool_description(service: ServiceDef, tool_name: &str) -> String {
    let description = match (service.id, tool_name) {
        ("careos-food-delivery", "list_restaurants") => {
            "列出附近可点餐的餐厅，适合推荐午餐、中午吃什么、点外卖、找餐厅、按口味或健康偏好筛选"
        }
        ("careos-food-delivery", "get_restaurant_menu") => {
            "查询餐厅菜单、套餐和菜品，适合继续查看吃什么、菜品详情、价格、低盐低糖等餐食选择"
        }
        ("careos-food-delivery", "create_order") => {
            "创建外卖订单和下单，仅在用户明确确认餐厅、菜品、地址后使用"
        }
        ("careos-food-delivery", "get_orders") => "查询外卖订单、订单状态、历史点餐记录",
        ("careos-food-delivery", "estimate_delivery") => "估算外卖配送时间、配送费和送达时间",
        ("careos-food-delivery", "get_addresses") => "查询点餐收货地址和默认地址",
        ("careos-ride-hailing", "list_car_types") => "列出可叫车车型，适合打车、叫车、出行用车",
        ("careos-ride-hailing", "estimate_price") => "估算打车价格、距离、到达时间和车型费用",
        ("careos-ride-hailing", "call_car") => {
            "呼叫网约车和创建打车订单，仅在用户确认上车点、目的地、车型后使用"
        }
        ("careos-health-supplement", "get_recommendations") => "根据健康档案、症状和目标推荐保健品",
        ("careos-medication-reminder", "get_today_schedule") => {
            "查询今天的用药计划、服药时间和提醒"
        }
        ("careos-consultation", "symptom_check") => {
            "根据症状做问诊分诊，适合不舒服、哪里疼、想咨询医生"
        }
        ("careos-housekeeping", "list_services") => "列出家政服务项目，适合保洁、维修、上门服务",
        ("careos-travel", "list_tours") => "列出旅游线路，适合旅行规划、出游推荐、旅游报价",
        ("careos-music", "list_songs") => "列出歌曲，适合听歌、播放音乐、找歌曲",
        ("careos-audiobook", "list_books") => "列出有声书，适合听书、播放有声读物、找书",
        ("careos-activities", "list_activities") => "列出社区活动，适合报名活动、查活动安排",
        ("careos-watch-health", "get_latest_health") => {
            "查询手表最新健康数据，包括心率、血压、步数、体温"
        }
        ("careos-emergency", "send_sos_alert") => "发送紧急 SOS 求助和紧急联系人通知",
        ("careos-phone-sms", "send_sms") => "发送短信",
        _ => return format!("{} tool exposed by {}.", service.description, service.id),
    };
    format!("{}。service={} tool={}", description, service.id, tool_name)
}

fn call_tool(service: ServiceDef, params: &Value) -> Result<Value> {
    let tool_name = params
        .get("name")
        .and_then(Value::as_str)
        .context("tools/call params missed name")?;
    if !service.tools.iter().any(|tool| *tool == tool_name) {
        bail!("unknown tool for {}: {tool_name}", service.id);
    }
    let arguments = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));
    Ok(json!({
        "content": [{
            "type": "text",
            "text": serde_json::to_string_pretty(&json!({
                "service": service.id,
                "tool": tool_name,
                "arguments": arguments,
                "data": mock_data(service, tool_name),
                "status": "ok"
            })).unwrap()
        }]
    }))
}

fn mock_data(service: ServiceDef, tool_name: &str) -> Value {
    match (service.id, tool_name) {
        ("careos-activities", "list_activities") => json!([
            {"id": "activity.calligraphy.001", "name": "书法班", "time": "2026-05-02 09:30", "location": "社区活动中心", "quota": 6},
            {"id": "activity.health-talk.002", "name": "健康睡眠讲座", "time": "2026-05-03 14:00", "location": "CareOS 活动厅", "quota": 20}
        ]),
        ("careos-watch-health", "get_latest_health") => {
            json!({"heart_rate": 76, "blood_pressure": "126/78", "steps": 4200, "temperature": 36.5})
        }
        ("careos-food-delivery", "list_restaurants") => {
            json!([{"restaurant_id": "rest.001", "name": "长者健康餐厅", "distance_km": 1.2}])
        }
        ("careos-food-delivery", "get_restaurant_menu") => {
            json!([{"meal_id": "meal.low-salt.001", "name": "低盐鸡肉套餐", "price": 32}])
        }
        ("careos-food-delivery", "create_order") => {
            json!({"order_id": "food.order.9001", "status": "created"})
        }
        ("careos-ride-hailing", "estimate_price") => {
            json!([{"car_type": "comfort", "price": 28, "eta_minutes": 6}])
        }
        ("careos-ride-hailing", "call_car") => {
            json!({"order_id": "ride.order.9001", "driver": "示例司机", "plate": "沪A-D1234"})
        }
        ("careos-medication-reminder", "get_today_schedule") => {
            json!([{"name": "Amlodipine", "time": "08:00", "dose": "1 tablet"}])
        }
        ("careos-health-supplement", "get_recommendations") => {
            json!([{"product_id": "supp.ca.001", "name": "钙 D 片", "price": 89}])
        }
        ("careos-housekeeping", "list_services") => {
            json!([{"service_id": "clean.basic", "name": "基础保洁", "price": 99}])
        }
        ("careos-music", "list_songs") => json!([{"song_id": "song.001", "name": "示例歌曲"}]),
        ("careos-audiobook", "list_books") => json!([{"book_id": "audio.001", "title": "西游记"}]),
        ("careos-finance", "risk_assessment") => {
            json!({"risk_level": "low", "note": "仅为演示数据"})
        }
        ("careos-travel", "list_tours") => {
            json!([{"tour_id": "trip.hangzhou.001", "name": "杭州一日游", "price": 199}])
        }
        ("careos-image-diagnosis", "submit_diagnosis") => {
            json!({"case_id": "img.case.001", "status": "submitted"})
        }
        ("careos-emergency", "send_sos_alert") => json!({"alert_id": "sos.001", "sent": true}),
        ("careos-phone-sms", "send_sms") => json!({"message_id": "sms.001", "sent": true}),
        ("careos-consultation", "symptom_check") => {
            json!({"triage": "general_consultation", "tips": ["记录症状", "必要时就医"]})
        }
        _ => json!({"message": "demo result"}),
    }
}

fn find_service(value: &str) -> Result<ServiceDef> {
    SERVICES
        .iter()
        .copied()
        .find(|service| service.id == value || service.slug == value)
        .with_context(|| format!("unknown CareOS MCP service: {value}"))
}
