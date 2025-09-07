use std::{collections::HashMap, error::Error, ops::Index, os::unix::ffi::OsStringExt, str::FromStr, sync::{mpsc::{self, Receiver, Sender}, Arc, Mutex, RwLock}, thread};

use poem::{error, get, http::{request, Uri}, listener::TcpListener, middleware::AddData, web, EndpointExt, Request, Route};
use poem_openapi::{param::Query, payload::{Json, PlainText}, ApiResponse, Object, OpenApi, OpenApiService};
use rhai::{Dynamic, Engine, EvalAltResult, AST};
use serde::Serialize;
use tokio::{fs};
use dashmap::DashMap;
use uuid::Uuid;
use tokio_cron_scheduler::{job, Job, JobScheduler, JobSchedulerError};
use cron::Schedule;

#[derive(ApiResponse)]
enum ApiError {
    
    #[oai(status = 500)]
    InternalError(PlainText<String>),

    #[oai(status = 400)] 
    RequestError(PlainText<String>),
    
    #[oai(status = 404)] 
    NotFoundError(PlainText<String>)
}

fn interal_error<E: std::fmt::Display>(err: E) -> ApiError {
    return ApiError::InternalError(PlainText(err.to_string()))
}
fn notfound_error<E: std::fmt::Display>(err: E) -> ApiError {
    return ApiError::NotFoundError(PlainText(err.to_string()))
}

fn badrequest_error<E: std::fmt::Display>(err: E) -> ApiError {
    return ApiError::RequestError(PlainText(err.to_string()))
}
struct AppState {
    scripts: DashMap<Uuid, String>,
    script_sender: std::sync::mpsc::Sender<(String, tokio::sync::oneshot::Sender<String>)>,
    script_scheduler: tokio::sync::mpsc::Sender<CronScript>
}




struct CronScript {
    cron: Schedule,
    script: String
}

#[derive(Object)]
struct ScriptRequest {
    cron: Option<String>,
    script: String
}

#[derive(Object)]
struct ScriptResponse {
    id: Uuid,
    script: String
}

#[derive(Object)]
struct ScriptPostResponse {
    id: Uuid,
    script: ScriptRequest
}

async fn run_script(script: String, script_sender: &Sender<(String, tokio::sync::oneshot::Sender<String>)>) -> Result<String, Box<dyn Error>> {
    let (res_sender, res_reciever) = tokio::sync::oneshot::channel::<String>();
    script_sender.send((script, res_sender))?;

    let res = res_reciever.await?;
    return Ok(res);
}

struct Api; 

#[OpenApi]
impl Api {

    #[oai(path = "/run-script", method = "get")]
    async fn run_script(&self, state: web::Data<&Arc<AppState>>, uuid: Query<Uuid>) -> Result<PlainText<String>, ApiError> {
        let script: String;
        {
            let kv = state.scripts.get(&uuid).ok_or(notfound_error(format!("Script id {} not found", uuid.0)))?;
            script = kv.value().to_owned()
        }

        let res = run_script(script, &state.script_sender).await.map_err(interal_error)?;
        return Ok(PlainText(res));
    }

    #[oai(path = "/scripts", method = "get")]
    async fn scripts(&self, state: web::Data<&Arc<AppState>>) -> Result<Json<Vec<ScriptResponse>>, ApiError> {
        let vec  = state.scripts.iter()
                                    .map(|kv| {
                                        return ScriptResponse { id: *kv.key(), script: kv.value().to_string() }
                                    })
                                    .collect::<Vec<ScriptResponse>>();
                                                            
        return Ok(Json(vec))
    }

    #[oai(path = "/script", method = "post")]
    async fn script_post(&self, state: web::Data<&Arc<AppState>>, payload: Json<ScriptRequest>) -> Result<Json<ScriptPostResponse>, ApiError> {
        {
            let engine = Engine::new() ;
            let _ = engine.compile(&payload.script).map_err(badrequest_error)?;
        }
        let id = Uuid::new_v4();
        let _ = state.scripts.insert(id, payload.script.clone());
        let cron = payload.cron.as_ref().map(|s| {Schedule::from_str(s).map_err(badrequest_error)});
        if let Some(cron) = cron {
            let script = cron.map(|c| {
                            CronScript {
                                cron: c,
                                script: payload.script.clone()
                            }
                        })?;
            state.script_scheduler.send(script).await.map_err(interal_error)?;

        }
        return Ok(Json(ScriptPostResponse {
            id: id,
            script: ScriptRequest {
                script: payload.script.clone(),
                cron: payload.cron.clone()
            }
        }));
       
    }

    #[oai(path = "/script", method = "delete")]
    async fn script_delete(&self, state: web::Data<&Arc<AppState>>, uuid: Query<Uuid>) -> Result<Json<ScriptResponse>, ApiError> {
        let removed = state.scripts.remove(&uuid).ok_or_else(|| notfound_error(format!("Script not found: {}", uuid.0)))?;
        return Ok(Json(ScriptResponse {
            id: removed.0,
            script: removed.1
        }));
    }
}

fn fetch(url: &str) -> Dynamic {
    let res = reqwest::blocking::get(url).and_then(|r| {r.text()});
    return match res {
        Ok(x) => Dynamic::from_str(&x).unwrap_or(Dynamic::UNIT),
        Err(x) => Dynamic::from_str(&(x.to_string())).unwrap_or(Dynamic::UNIT)
    }
}
#[tokio::main]
async fn main() -> Result<(), std::io::Error> {
    let (script_sender, script_reciever) =  mpsc::channel::<(String, tokio::sync::oneshot::Sender<String>)>();
    let (scheduler_sender, mut scheduler_reciever) = tokio::sync::mpsc::channel(500);
    let state = Arc::new(AppState{
        scripts: DashMap::new(),
        script_sender: script_sender,
        script_scheduler: scheduler_sender
    });
    let schedulers_state = state.clone();
    tokio::spawn(async move {
        let sched = JobScheduler::new().await.expect("Nani????");
        sched.start().await.expect("NANNNNNNNNIIIIII");
        while let Some(script) = scheduler_reciever.recv().await {
            let schedulers_state_cloned = schedulers_state.clone();
            let script_clone = CronScript {
                cron: script.cron.clone(),
                script: script.script.clone(),
            };
            let job_res = Job::new_async(script_clone.cron, move |_uuid, _l| {
                println!("Job id: {}", _uuid);
                let schedulers_state = schedulers_state_cloned.clone();
                let script_inner = script_clone.script.clone();
                return Box::pin(async move {
                    let _ = run_script(script_inner, &schedulers_state.script_sender).await;
                })
            });
            match job_res {
                Ok(job) => {
                    if let Err(e) = sched.add(job).await {
                        eprintln!("Scheduling error: {}", e);
                    }
                },
                Err(e) => eprintln!("Scheduling error: {}", e)
            }

    }});

    let handle = rayon::spawn(move || {

        while let Ok((script, sender)) = script_reciever.recv() {
            rayon::spawn(move || {
                let mut engine = Engine::new();
                engine.register_fn("fetch", fetch);
                let res = match engine.eval(&script) {
                    Ok(x) => sender.send(x),
                    Err(x) => sender.send(x.to_string())
                };
            });
        }
    });

    let api_service =
        OpenApiService::new(Api, "Hello World", "1.0")
                    .server("http://localhost:3000/api")
                    .server("https://rhai-runner-27790705784.europe-north2.run.app/api");
    let ui = api_service.scalar();
    let app = Route::new()
                                                    .nest("/api", api_service)
                                                    .nest("/", ui)
                                                    // .at("/openapi.json", get(|| async move { spec.clone() }))
                                                    .with(AddData::new(state));
    poem::Server::new(TcpListener::bind("0.0.0.0:3000"))
        .run(app)
        .await
}