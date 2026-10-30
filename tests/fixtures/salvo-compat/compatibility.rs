use salvo_core::http::header::{COOKIE, SET_COOKIE};
use salvo_core::prelude::*;
use salvo_core::test::{ResponseExt, TestClient};
use saysion::{MemoryStore, SessionStore};
use saysion_salvo_compat::{SessionDepotExt, SessionHandler};

#[handler]
async fn write_session(depot: &mut Depot, res: &mut Response) {
    depot.session_mut().unwrap().insert("user", 42_u32).unwrap();
    res.render("saved");
}

#[handler]
async fn read_session(depot: &mut Depot, res: &mut Response) {
    let value = depot.session().unwrap().get::<u32>("user");
    res.render(value.map_or_else(|| "anonymous".to_owned(), |id| id.to_string()));
}

#[handler]
async fn destroy_session(depot: &mut Depot, res: &mut Response) {
    depot.session_mut().unwrap().destroy();
    res.render("destroyed");
}

#[tokio::test]
async fn actual_middleware_accepts_current_store_and_persists_reads_and_destroys() {
    let store = MemoryStore::new();
    let handler = SessionHandler::builder(store.clone(), &[42; 64])
        .build()
        .unwrap();
    let service = Service::new(
        Router::new()
            .hoop(handler)
            .push(Router::with_path("write").get(write_session))
            .push(Router::with_path("read").get(read_session))
            .push(Router::with_path("destroy").get(destroy_session)),
    );
    let created = TestClient::get("http://localhost/write")
        .send(&service)
        .await;
    assert_eq!(created.status_code.unwrap().as_u16(), 200);
    let cookie = created.headers().get(SET_COOKIE).unwrap().clone();
    assert_eq!(store.count().await, 1);
    let mut loaded = TestClient::get("http://localhost/read")
        .add_header(COOKIE, cookie.clone(), true)
        .send(&service)
        .await;
    assert_eq!(loaded.take_string().await.unwrap(), "42");
    let deleted = TestClient::get("http://localhost/destroy")
        .add_header(COOKIE, cookie.clone(), true)
        .send(&service)
        .await;
    assert!(deleted.headers().contains_key(SET_COOKIE));
    assert_eq!(store.count().await, 0);
    let mut replay = TestClient::get("http://localhost/read")
        .add_header(COOKIE, cookie, true)
        .send(&service)
        .await;
    assert_eq!(replay.take_string().await.unwrap(), "anonymous");
    // Also type-check the re-exported middleware trait against this exact store.
    fn same_store_trait<S: saysion_salvo_compat::SessionStore + SessionStore>(_: &S) {}
    same_store_trait(&store);
}
