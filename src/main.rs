use std::{collections::HashMap, error::Error, ops::Index, os::unix::ffi::OsStringExt, str::FromStr, sync::{mpsc::{self, Receiver}, Arc, Mutex, RwLock}, thread};

use poem::{error, get, http::{request, Uri}, listener::TcpListener, middleware::AddData, web, EndpointExt, Request, Route};
use poem_openapi::{param::Query, payload::PlainText, OpenApi, OpenApiService, ApiResponse};
use rhai::{Dynamic, Engine, EvalAltResult, AST};
use tokio::fs;

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
    scripts: RwLock<Vec<String>>,
    script_sender: std::sync::mpsc::Sender<(String, tokio::sync::oneshot::Sender<String>)>
}

async fn fetch_factory() -> impl Fn(&str) -> Dynamic {
    let (url_sender, mut url_reciever) = tokio::sync::mpsc::channel::<String>(1000);
    let (response_sender,response_reciever) = mpsc::channel::<String>();

    let handle = tokio::spawn(async move {
        let client = reqwest::Client::new();
        while let Some(url) = url_reciever.recv().await {
            let resp = client.get(url).send().await.unwrap();
            let text = resp.text().await.unwrap();
            response_sender.send(text);
        }
    });


    return move |str| {

        let _ = url_sender.try_send(str.to_string());
        thread::yield_now();
        match response_reciever.recv() {
            Ok(x) => x.into(),
            Err(e) => panic!("Failed to receive response: {}", e),
        }
    };
}

struct Api; 

#[OpenApi]
impl Api {


    #[oai(path = "/hello", method = "get")]
    async fn index(&self, name: Query<Option<String>>) -> PlainText<String> {
        match name.0 {
            Some(name) => PlainText(format!("hello, {}!", name)),
            None => PlainText("hello!".to_string()),
        }
    }

    #[oai(path = "/run-script", method = "get")]
    async fn run_script(&self, state: web::Data<&Arc<AppState>>, idx: Query<usize>) -> Result<PlainText<String>, ApiError> {
        let fetch = fetch_factory().await;
        let script: String;
        {

            let vector = state.scripts.read().map_err(interal_error)?;
            let index = idx.abs_diff(0);
            let s = &vector.get(index).ok_or_else(|| {notfound_error(format!("Script not found at index: {}", index))})?;
            script = s.to_string();
        }

        let (res_sender, res_reciever) = tokio::sync::oneshot::channel::<String>();
        state.script_sender.send((script, res_sender)).expect("surely this doesnt error");

        let res = res_reciever.await.map_err(interal_error)?;
        return Ok(PlainText(res));
    }

    #[oai(path = "/scripts", method = "get")]
    async fn scripts(&self, state: web::Data<&Arc<AppState>>) -> Result<PlainText<String>, ApiError> {
        let mut i = 0;
        let vec  = state.scripts.read()
                                    .map_err(interal_error)?
                                    .iter().map(|s| {
                                        i += 1;
                                        return format!("Script {}: {}", i - 1, s)
                                    })
                                    .collect::<Vec<String>>();
                                                            
        let res = vec.join("\n-----------------------------------------------------------------------\n");
        
        return Ok(PlainText(res))
    }

    #[oai(path = "/script", method = "post")]
    async fn script_post(&self, state: web::Data<&Arc<AppState>>, payload: PlainText<String>) -> Result<PlainText<String>, ApiError> {
        let engine = Engine::new() ;
        let _ = engine.compile(&payload.0).map_err(badrequest_error)?;
        let mut vec = state.scripts.write().map_err(interal_error)?;
        vec.push(payload.0);
        let size = vec.len();
        return Ok(PlainText(format!("Index of script: {}", size - 1)));
       
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
    let state = Arc::new(AppState{
        scripts: RwLock::new(vec![]),
        script_sender: script_sender
    });

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
        OpenApiService::new(Api, "Hello World", "1.0").server("http://localhost:3000/api");
    let ui = api_service.swagger_ui();
    let spec = api_service.spec();
    fs::write("openapi.json", spec).await.expect("Unable to write file");
    let app = Route::new()
                                                    .nest("/api", api_service)
                                                    .nest("/", ui)
                                                    // .at("/openapi.json", get(|| async move { spec.clone() }))
                                                    .with(AddData::new(state));

    poem::Server::new(TcpListener::bind("0.0.0.0:3000"))
        .run(app)
        .await
}