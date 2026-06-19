use std::path::Path;

use anyhow::{Context, Result};
use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode, header},
};
use file_hub::{auth::AuthState, config::AppConfig, http::build_router_with_bootstrap_report};
use sqlx::{SqlitePool, sqlite::SqliteConnectOptions};
use tempfile::TempDir;
use tokio::fs;
use tower::ServiceExt;

#[tokio::test]
async fn test_should_create_admin_bootstrap_password_on_first_startup() -> Result<()> {
    let storage_root = tempfile::tempdir().context("create temporary storage root")?;
    let config = config_from_storage_root(storage_root.path()).await?;

    let built = build_router_with_bootstrap_report(config)
        .await
        .context("build router with bootstrap report")?;
    let bootstrap = built
        .bootstrap_password()
        .context("first startup should report bootstrap password")?;

    assert_eq!(bootstrap.username(), "admin");
    assert!(bootstrap.plaintext_password().len() >= 16);
    assert!(
        !bootstrap
            .password_hash_for_debug()
            .contains(bootstrap.plaintext_password())
    );
    Ok(())
}

#[tokio::test]
async fn test_should_login_with_bootstrap_password_and_show_admin_identity() -> Result<()> {
    let storage_root = tempfile::tempdir().context("create temporary storage root")?;
    let config = config_from_storage_root(storage_root.path()).await?;
    let database_path = config.database_path().to_owned();
    let built = build_router_with_bootstrap_report(config)
        .await
        .context("build router with bootstrap report")?;
    let password = built
        .bootstrap_password()
        .context("first startup should report bootstrap password")?
        .plaintext_password()
        .to_owned();
    let app = built.into_router();

    let login = app
        .clone()
        .oneshot(json_request(
            "/api/login",
            serde_json::json!({
                "username": "admin",
                "password": password,
            }),
        )?)
        .await
        .context("send login request")?;

    assert_eq!(login.status(), StatusCode::NO_CONTENT);
    let session_cookie = login
        .headers()
        .get(header::SET_COOKIE)
        .context("login should set a session cookie")?
        .to_str()
        .context("session cookie should be ASCII")?
        .to_owned();
    assert!(session_cookie.contains("HttpOnly"));
    assert!(session_cookie.contains("Secure"));
    assert!(session_cookie.contains("SameSite=Lax"));
    let session_token = session_token_from_cookie(&session_cookie)?;
    assert_eq!(session_token.len(), 64);
    assert!(session_token.bytes().all(|byte| byte.is_ascii_hexdigit()));

    let stored_token_hash = stored_session_token_hash(&database_path).await?;
    assert_ne!(stored_token_hash, session_token);
    assert_eq!(stored_token_hash.len(), 64);
    assert!(
        stored_token_hash
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    );

    let identity = app
        .oneshot(
            Request::builder()
                .uri("/api/identity")
                .header(header::COOKIE, session_cookie)
                .body(Body::empty())
                .context("build identity request")?,
        )
        .await
        .context("send identity request")?;

    assert_eq!(identity.status(), StatusCode::OK);
    let identity = response_json(identity)
        .await
        .context("read identity response")?;
    assert_eq!(
        identity.pointer("/username"),
        Some(&serde_json::Value::String("admin".to_owned()))
    );
    assert_eq!(
        identity.pointer("/actions/passwordChange"),
        Some(&serde_json::Value::Bool(true))
    );
    assert_eq!(
        identity.pointer("/actions/logout"),
        Some(&serde_json::Value::Bool(true))
    );
    assert_eq!(
        identity.pointer("/actions/console"),
        Some(&serde_json::Value::Bool(true))
    );
    Ok(())
}

#[tokio::test]
async fn test_should_logout_and_return_to_anonymous_identity() -> Result<()> {
    let storage_root = tempfile::tempdir().context("create temporary storage root")?;
    let config = config_from_storage_root(storage_root.path()).await?;
    let built = build_router_with_bootstrap_report(config)
        .await
        .context("build router with bootstrap report")?;
    let password = built
        .bootstrap_password()
        .context("first startup should report bootstrap password")?
        .plaintext_password()
        .to_owned();
    let app = built.into_router();
    let session_cookie = login_session_cookie(app.clone(), "admin", &password).await?;

    let logout = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/logout")
                .header(header::COOKIE, &session_cookie)
                .body(Body::empty())
                .context("build logout request")?,
        )
        .await
        .context("send logout request")?;

    assert_eq!(logout.status(), StatusCode::NO_CONTENT);
    assert!(
        logout
            .headers()
            .get(header::SET_COOKIE)
            .context("logout should clear session cookie")?
            .to_str()
            .context("logout cookie should be ASCII")?
            .contains("Max-Age=0")
    );

    let identity = identity_with_cookie(app, &session_cookie).await?;
    assert_eq!(
        identity.pointer("/authenticated"),
        Some(&serde_json::Value::Bool(false))
    );
    assert_eq!(
        identity.pointer("/actions/login"),
        Some(&serde_json::Value::Bool(true))
    );
    Ok(())
}

#[tokio::test]
async fn test_should_reject_oversized_passwords_without_revoking_current_session() -> Result<()> {
    let storage_root = tempfile::tempdir().context("create temporary storage root")?;
    let config = config_from_storage_root(storage_root.path()).await?;
    let built = build_router_with_bootstrap_report(config)
        .await
        .context("build router with bootstrap report")?;
    let bootstrap_password = built
        .bootstrap_password()
        .context("first startup should report bootstrap password")?
        .plaintext_password()
        .to_owned();
    let app = built.into_router();
    let session_cookie = login_session_cookie(app.clone(), "admin", &bootstrap_password).await?;
    let oversized_password = "a".repeat(257);

    let oversized_login = app
        .clone()
        .oneshot(json_request(
            "/api/login",
            serde_json::json!({
                "username": "admin",
                "password": &oversized_password,
            }),
        )?)
        .await
        .context("send oversized login request")?;
    assert_eq!(oversized_login.status(), StatusCode::UNAUTHORIZED);

    let oversized_change = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/password")
                .header(header::COOKIE, &session_cookie)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "oldPassword": bootstrap_password,
                        "newPassword": &oversized_password,
                    })
                    .to_string(),
                ))
                .context("build oversized password change request")?,
        )
        .await
        .context("send oversized password change request")?;
    assert_eq!(oversized_change.status(), StatusCode::UNAUTHORIZED);

    let identity = identity_with_cookie(app, &session_cookie).await?;
    assert_eq!(
        identity.pointer("/authenticated"),
        Some(&serde_json::Value::Bool(true))
    );
    Ok(())
}

#[tokio::test]
async fn test_should_change_admin_password_revoke_sessions_and_stop_bootstrap_logging() -> Result<()>
{
    let storage_root = tempfile::tempdir().context("create temporary storage root")?;
    let config = config_from_storage_root(storage_root.path()).await?;
    let built = build_router_with_bootstrap_report(config.clone())
        .await
        .context("build router with bootstrap report")?;
    let bootstrap_password = built
        .bootstrap_password()
        .context("first startup should report bootstrap password")?
        .plaintext_password()
        .to_owned();
    let app = built.into_router();
    let session_cookie = login_session_cookie(app.clone(), "admin", &bootstrap_password).await?;

    let password_change = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/password")
                .header(header::COOKIE, &session_cookie)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "oldPassword": bootstrap_password,
                        "newPassword": "changed-admin-password",
                    })
                    .to_string(),
                ))
                .context("build password change request")?,
        )
        .await
        .context("send password change request")?;

    assert_eq!(password_change.status(), StatusCode::NO_CONTENT);
    assert!(
        password_change
            .headers()
            .get(header::SET_COOKIE)
            .context("password change should clear current session")?
            .to_str()
            .context("password change cookie should be ASCII")?
            .contains("Max-Age=0")
    );

    let old_identity = identity_with_cookie(app.clone(), &session_cookie).await?;
    assert_eq!(
        old_identity.pointer("/authenticated"),
        Some(&serde_json::Value::Bool(false))
    );

    let restarted = build_router_with_bootstrap_report(config)
        .await
        .context("rebuild router after admin password change")?;
    assert!(restarted.bootstrap_password().is_none());

    let new_session =
        login_session_cookie(restarted.into_router(), "admin", "changed-admin-password").await?;
    assert!(new_session.contains("fh_session="));
    Ok(())
}

#[tokio::test]
async fn test_should_regenerate_bootstrap_password_on_restart_before_admin_changes_it() -> Result<()>
{
    let storage_root = tempfile::tempdir().context("create temporary storage root")?;
    let config = config_from_storage_root(storage_root.path()).await?;

    let first = build_router_with_bootstrap_report(config.clone())
        .await
        .context("build first router")?;
    let first_password = first
        .bootstrap_password()
        .context("first startup should report bootstrap password")?
        .plaintext_password()
        .to_owned();

    let second = build_router_with_bootstrap_report(config)
        .await
        .context("build second router")?;
    let second_password = second
        .bootstrap_password()
        .context("second startup should report regenerated bootstrap password")?
        .plaintext_password()
        .to_owned();

    assert_ne!(first_password, second_password);

    let stale_login = second
        .into_router()
        .clone()
        .oneshot(json_request(
            "/api/login",
            serde_json::json!({
                "username": "admin",
                "password": first_password,
            }),
        )?)
        .await
        .context("send stale bootstrap login request")?;

    assert_eq!(stale_login.status(), StatusCode::UNAUTHORIZED);
    Ok(())
}

#[tokio::test]
async fn test_should_change_ordinary_user_password_and_revoke_old_sessions() -> Result<()> {
    let storage_root = tempfile::tempdir().context("create temporary storage root")?;
    let config = config_from_storage_root(storage_root.path()).await?;
    let built = build_router_with_bootstrap_report(config.clone())
        .await
        .context("build router with bootstrap report")?;
    let auth = AuthState::connect_existing(config.database_path())
        .await
        .context("connect auth state")?;
    auth.create_user("Alice", "alice-old-password")
        .await
        .context("create ordinary user")?;
    let app = built.into_router();

    let session_cookie = login_session_cookie(app.clone(), "Alice", "alice-old-password").await?;
    let identity = identity_with_cookie(app.clone(), &session_cookie).await?;
    assert_eq!(
        identity.pointer("/username"),
        Some(&serde_json::Value::String("Alice".to_owned()))
    );
    assert_eq!(
        identity.pointer("/actions/console"),
        Some(&serde_json::Value::Bool(false))
    );

    let password_change = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/password")
                .header(header::COOKIE, &session_cookie)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "oldPassword": "alice-old-password",
                        "newPassword": "alice-new-password",
                    })
                    .to_string(),
                ))
                .context("build ordinary password change request")?,
        )
        .await
        .context("send ordinary password change request")?;

    assert_eq!(password_change.status(), StatusCode::NO_CONTENT);
    let old_identity = identity_with_cookie(app.clone(), &session_cookie).await?;
    assert_eq!(
        old_identity.pointer("/authenticated"),
        Some(&serde_json::Value::Bool(false))
    );

    let new_session = login_session_cookie(app, "Alice", "alice-new-password").await?;
    assert!(new_session.contains("fh_session="));
    Ok(())
}

#[tokio::test]
async fn test_should_reject_console_attempts_to_create_delete_rename_or_replace_administrator()
-> Result<()> {
    let storage_root = tempfile::tempdir().context("create temporary storage root")?;
    let config = config_from_storage_root(storage_root.path()).await?;
    let built = build_router_with_bootstrap_report(config)
        .await
        .context("build router with bootstrap report")?;
    let password = built
        .bootstrap_password()
        .context("first startup should report bootstrap password")?
        .plaintext_password()
        .to_owned();
    let app = built.into_router();
    let session_cookie = login_session_cookie(app.clone(), "admin", &password).await?;

    let create_admin = app
        .clone()
        .oneshot(json_request_with_cookie(
            "POST",
            "/api/console/users",
            serde_json::json!({
                "username": "admin",
                "password": "another-password",
            }),
            &session_cookie,
        )?)
        .await
        .context("send create admin request")?;
    assert_error(
        create_admin,
        StatusCode::BAD_REQUEST,
        "reserved_administrator",
    )
    .await?;

    let delete_admin = console_request("DELETE", "/api/console/users/admin", &session_cookie)?;
    let delete_admin = app
        .clone()
        .oneshot(delete_admin)
        .await
        .context("send delete admin request")?;
    assert_error(
        delete_admin,
        StatusCode::BAD_REQUEST,
        "reserved_administrator",
    )
    .await?;

    let rename_admin = app
        .clone()
        .oneshot(json_request_with_cookie(
            "PATCH",
            "/api/console/users/admin",
            serde_json::json!({
                "username": "root",
            }),
            &session_cookie,
        )?)
        .await
        .context("send rename admin request")?;
    assert_error(
        rename_admin,
        StatusCode::BAD_REQUEST,
        "reserved_administrator",
    )
    .await?;

    let replace_admin = app
        .oneshot(json_request_with_cookie(
            "PUT",
            "/api/console/users/admin",
            serde_json::json!({
                "username": "root",
                "password": "replacement-password",
            }),
            &session_cookie,
        )?)
        .await
        .context("send replace admin request")?;
    assert_error(
        replace_admin,
        StatusCode::BAD_REQUEST,
        "reserved_administrator",
    )
    .await?;
    Ok(())
}

#[tokio::test]
async fn test_should_create_ordinary_user_through_console_api_and_log_in_with_permissions()
-> Result<()> {
    let storage_root = tempfile::tempdir().context("create temporary storage root")?;
    let config = config_from_storage_root(storage_root.path()).await?;
    let built = build_router_with_bootstrap_report(config)
        .await
        .context("build router with bootstrap report")?;
    let password = built
        .bootstrap_password()
        .context("first startup should report bootstrap password")?
        .plaintext_password()
        .to_owned();
    let app = built.into_router();
    let admin_cookie = login_session_cookie(app.clone(), "admin", &password).await?;

    let create_user = app
        .clone()
        .oneshot(json_request_with_cookie(
            "POST",
            "/api/console/users",
            serde_json::json!({
                "username": "Alice-1",
                "password": "alice-password",
                "permissions": {
                    "upload": true,
                    "rename": false,
                    "delete": true,
                },
            }),
            &admin_cookie,
        )?)
        .await
        .context("send console create user request")?;

    assert_eq!(create_user.status(), StatusCode::CREATED);
    let created = response_json(create_user).await?;
    assert_eq!(
        created.pointer("/username"),
        Some(&serde_json::Value::String("Alice-1".to_owned()))
    );
    assert_eq!(
        created.pointer("/permissions/upload"),
        Some(&serde_json::Value::Bool(true))
    );
    assert_eq!(
        created.pointer("/permissions/rename"),
        Some(&serde_json::Value::Bool(false))
    );
    assert_eq!(
        created.pointer("/permissions/delete"),
        Some(&serde_json::Value::Bool(true))
    );

    let user_cookie = login_session_cookie(app.clone(), "Alice-1", "alice-password").await?;
    let identity = identity_with_cookie(app, &user_cookie).await?;
    assert_eq!(
        identity.pointer("/username"),
        Some(&serde_json::Value::String("Alice-1".to_owned()))
    );
    assert_eq!(
        identity.pointer("/actions/console"),
        Some(&serde_json::Value::Bool(false))
    );
    assert_eq!(
        identity.pointer("/actions/upload"),
        Some(&serde_json::Value::Bool(true))
    );
    assert_eq!(
        identity.pointer("/actions/rename"),
        Some(&serde_json::Value::Bool(false))
    );
    assert_eq!(
        identity.pointer("/actions/delete"),
        Some(&serde_json::Value::Bool(true))
    );
    Ok(())
}

#[tokio::test]
async fn test_should_allow_only_administrator_to_access_console_api_and_view() -> Result<()> {
    let storage_root = tempfile::tempdir().context("create temporary storage root")?;
    let config = config_from_storage_root(storage_root.path()).await?;
    let built = build_router_with_bootstrap_report(config)
        .await
        .context("build router with bootstrap report")?;
    let password = built
        .bootstrap_password()
        .context("first startup should report bootstrap password")?
        .plaintext_password()
        .to_owned();
    let app = built.into_router();
    let admin_cookie = login_session_cookie(app.clone(), "admin", &password).await?;

    let anonymous_api = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/console/users")
                .body(Body::empty())
                .context("build anonymous console API request")?,
        )
        .await
        .context("send anonymous console API request")?;
    assert_error(
        anonymous_api,
        StatusCode::UNAUTHORIZED,
        "authentication_required",
    )
    .await?;

    let anonymous_view = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/console")
                .body(Body::empty())
                .context("build anonymous console view request")?,
        )
        .await
        .context("send anonymous console view request")?;
    assert_error(
        anonymous_view,
        StatusCode::UNAUTHORIZED,
        "authentication_required",
    )
    .await?;

    let create_user = app
        .clone()
        .oneshot(json_request_with_cookie(
            "POST",
            "/api/console/users",
            serde_json::json!({
                "username": "console-user",
                "password": "console-user-password",
                "permissions": {
                    "upload": false,
                    "rename": false,
                    "delete": false,
                },
            }),
            &admin_cookie,
        )?)
        .await
        .context("send create ordinary user request")?;
    assert_eq!(create_user.status(), StatusCode::CREATED);
    let user_cookie = login_session_cookie(app.clone(), "console-user", "console-user-password")
        .await
        .context("login ordinary user")?;

    let ordinary_api = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/console/users")
                .header(header::COOKIE, &user_cookie)
                .body(Body::empty())
                .context("build ordinary console API request")?,
        )
        .await
        .context("send ordinary console API request")?;
    assert_error(ordinary_api, StatusCode::FORBIDDEN, "forbidden").await?;

    let ordinary_view = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/console")
                .header(header::COOKIE, &user_cookie)
                .body(Body::empty())
                .context("build ordinary console view request")?,
        )
        .await
        .context("send ordinary console view request")?;
    assert_error(ordinary_view, StatusCode::FORBIDDEN, "forbidden").await?;

    let admin_api = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/console/users")
                .header(header::COOKIE, &admin_cookie)
                .body(Body::empty())
                .context("build admin console API request")?,
        )
        .await
        .context("send admin console API request")?;
    assert_eq!(admin_api.status(), StatusCode::OK);

    let admin_view = app
        .oneshot(
            Request::builder()
                .uri("/console")
                .header(header::COOKIE, &admin_cookie)
                .body(Body::empty())
                .context("build admin console view request")?,
        )
        .await
        .context("send admin console view request")?;
    assert_eq!(admin_view.status(), StatusCode::OK);
    let body = to_bytes(admin_view.into_body(), usize::MAX)
        .await
        .context("read console view body")?;
    let body = String::from_utf8(body.to_vec()).context("console view must be UTF-8")?;
    assert!(body.contains("<div id=\"app\"></div>"));
    assert!(body.contains("nomodule"));
    Ok(())
}

#[tokio::test]
async fn test_should_authorize_console_requests_before_parsing_json_bodies() -> Result<()> {
    let storage_root = tempfile::tempdir().context("create temporary storage root")?;
    let config = config_from_storage_root(storage_root.path()).await?;
    let built = build_router_with_bootstrap_report(config)
        .await
        .context("build router with bootstrap report")?;
    let password = built
        .bootstrap_password()
        .context("first startup should report bootstrap password")?
        .plaintext_password()
        .to_owned();
    let app = built.into_router();
    let admin_cookie = login_session_cookie(app.clone(), "admin", &password).await?;
    let create_user = app
        .clone()
        .oneshot(json_request_with_cookie(
            "POST",
            "/api/console/users",
            serde_json::json!({
                "username": "ordinary-user",
                "password": "ordinary-password",
            }),
            &admin_cookie,
        )?)
        .await
        .context("create ordinary user")?;
    assert_eq!(create_user.status(), StatusCode::CREATED);
    let ordinary_cookie =
        login_session_cookie(app.clone(), "ordinary-user", "ordinary-password").await?;

    let anonymous = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/console/users")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from("{"))
                .context("build malformed anonymous console request")?,
        )
        .await
        .context("send malformed anonymous console request")?;
    assert_error(
        anonymous,
        StatusCode::UNAUTHORIZED,
        "authentication_required",
    )
    .await?;

    let ordinary = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/console/users")
                .header(header::CONTENT_TYPE, "application/json")
                .header(header::COOKIE, ordinary_cookie)
                .body(Body::from("{"))
                .context("build malformed ordinary console request")?,
        )
        .await
        .context("send malformed ordinary console request")?;
    assert_error(ordinary, StatusCode::FORBIDDEN, "forbidden").await?;
    Ok(())
}

#[tokio::test]
async fn test_should_validate_console_usernames_and_preserve_display_casing() -> Result<()> {
    let storage_root = tempfile::tempdir().context("create temporary storage root")?;
    let config = config_from_storage_root(storage_root.path()).await?;
    let built = build_router_with_bootstrap_report(config)
        .await
        .context("build router with bootstrap report")?;
    let password = built
        .bootstrap_password()
        .context("first startup should report bootstrap password")?
        .plaintext_password()
        .to_owned();
    let app = built.into_router();
    let admin_cookie = login_session_cookie(app.clone(), "admin", &password).await?;

    let long_username = "a".repeat(65);
    for invalid_username in [
        "",
        "not allowed",
        "slash/name",
        "ümlaut",
        long_username.as_str(),
    ] {
        let response = app
            .clone()
            .oneshot(json_request_with_cookie(
                "POST",
                "/api/console/users",
                serde_json::json!({
                    "username": invalid_username,
                    "password": "valid-password",
                    "permissions": {
                        "upload": false,
                        "rename": false,
                        "delete": false,
                    },
                }),
                &admin_cookie,
            )?)
            .await
            .context("send invalid username create request")?;
        assert_error(response, StatusCode::BAD_REQUEST, "invalid_username").await?;
    }

    let create = app
        .clone()
        .oneshot(json_request_with_cookie(
            "POST",
            "/api/console/users",
            serde_json::json!({
                "username": "Mixed_Case-1",
                "password": "valid-password",
                "permissions": {
                    "upload": false,
                    "rename": false,
                    "delete": false,
                },
            }),
            &admin_cookie,
        )?)
        .await
        .context("send valid mixed-case username create request")?;
    assert_eq!(create.status(), StatusCode::CREATED);

    let duplicate = app
        .clone()
        .oneshot(json_request_with_cookie(
            "POST",
            "/api/console/users",
            serde_json::json!({
                "username": "mixed_case-1",
                "password": "another-password",
                "permissions": {
                    "upload": false,
                    "rename": false,
                    "delete": false,
                },
            }),
            &admin_cookie,
        )?)
        .await
        .context("send duplicate username create request")?;
    assert_error(duplicate, StatusCode::CONFLICT, "username_conflict").await?;

    let list = app
        .oneshot(
            Request::builder()
                .uri("/api/console/users")
                .header(header::COOKIE, &admin_cookie)
                .body(Body::empty())
                .context("build console list users request")?,
        )
        .await
        .context("send console list users request")?;
    assert_eq!(list.status(), StatusCode::OK);
    let body = response_json(list).await?;
    assert_eq!(
        body.pointer("/users/0/username"),
        Some(&serde_json::Value::String("Mixed_Case-1".to_owned()))
    );
    Ok(())
}

#[tokio::test]
async fn test_should_validate_console_initial_password_length_without_composition_rules()
-> Result<()> {
    let storage_root = tempfile::tempdir().context("create temporary storage root")?;
    let config = config_from_storage_root(storage_root.path()).await?;
    let built = build_router_with_bootstrap_report(config)
        .await
        .context("build router with bootstrap report")?;
    let password = built
        .bootstrap_password()
        .context("first startup should report bootstrap password")?
        .plaintext_password()
        .to_owned();
    let app = built.into_router();
    let admin_cookie = login_session_cookie(app.clone(), "admin", &password).await?;

    let short_password = app
        .clone()
        .oneshot(json_request_with_cookie(
            "POST",
            "/api/console/users",
            serde_json::json!({
                "username": "short-password-user",
                "password": "1234567",
                "permissions": {
                    "upload": false,
                    "rename": false,
                    "delete": false,
                },
            }),
            &admin_cookie,
        )?)
        .await
        .context("send short password create request")?;
    assert_error(short_password, StatusCode::BAD_REQUEST, "invalid_password").await?;

    let short_unicode_password = app
        .clone()
        .oneshot(json_request_with_cookie(
            "POST",
            "/api/console/users",
            serde_json::json!({
                "username": "short-unicode-password-user",
                "password": "密碼密碼密碼密",
            }),
            &admin_cookie,
        )?)
        .await
        .context("send short Unicode password create request")?;
    assert_error(
        short_unicode_password,
        StatusCode::BAD_REQUEST,
        "invalid_password",
    )
    .await?;

    let simple_password = app
        .clone()
        .oneshot(json_request_with_cookie(
            "POST",
            "/api/console/users",
            serde_json::json!({
                "username": "simple-password-user",
                "password": "aaaaaaaa",
                "permissions": {
                    "upload": false,
                    "rename": false,
                    "delete": false,
                },
            }),
            &admin_cookie,
        )?)
        .await
        .context("send simple password create request")?;
    assert_eq!(simple_password.status(), StatusCode::CREATED);

    let unicode_password = app
        .clone()
        .oneshot(json_request_with_cookie(
            "POST",
            "/api/console/users",
            serde_json::json!({
                "username": "unicode-password-user",
                "password": "密碼密碼密碼密碼",
            }),
            &admin_cookie,
        )?)
        .await
        .context("send eight-character Unicode password create request")?;
    assert_eq!(unicode_password.status(), StatusCode::CREATED);

    let user_cookie = login_session_cookie(app, "simple-password-user", "aaaaaaaa").await?;
    assert!(user_cookie.contains("fh_session="));
    Ok(())
}

#[tokio::test]
async fn test_should_default_new_users_to_no_write_permissions_and_edit_each_permission()
-> Result<()> {
    let storage_root = tempfile::tempdir().context("create temporary storage root")?;
    let config = config_from_storage_root(storage_root.path()).await?;
    let built = build_router_with_bootstrap_report(config)
        .await
        .context("build router with bootstrap report")?;
    let password = built
        .bootstrap_password()
        .context("first startup should report bootstrap password")?
        .plaintext_password()
        .to_owned();
    let app = built.into_router();
    let admin_cookie = login_session_cookie(app.clone(), "admin", &password).await?;

    let create_user = app
        .clone()
        .oneshot(json_request_with_cookie(
            "POST",
            "/api/console/users",
            serde_json::json!({
                "username": "permission-user",
                "password": "permission-password",
            }),
            &admin_cookie,
        )?)
        .await
        .context("send default-permission user create request")?;
    assert_eq!(create_user.status(), StatusCode::CREATED);
    let created = response_json(create_user).await?;
    assert_eq!(
        created.pointer("/permissions/upload"),
        Some(&serde_json::Value::Bool(false))
    );
    assert_eq!(
        created.pointer("/permissions/rename"),
        Some(&serde_json::Value::Bool(false))
    );
    assert_eq!(
        created.pointer("/permissions/delete"),
        Some(&serde_json::Value::Bool(false))
    );

    let user_cookie =
        login_session_cookie(app.clone(), "permission-user", "permission-password").await?;
    let default_identity = identity_with_cookie(app.clone(), &user_cookie).await?;
    assert_eq!(
        default_identity.pointer("/actions/upload"),
        Some(&serde_json::Value::Bool(false))
    );
    assert_eq!(
        default_identity.pointer("/actions/rename"),
        Some(&serde_json::Value::Bool(false))
    );
    assert_eq!(
        default_identity.pointer("/actions/delete"),
        Some(&serde_json::Value::Bool(false))
    );

    let edited = app
        .clone()
        .oneshot(json_request_with_cookie(
            "PATCH",
            "/api/console/users/permission-user/permissions",
            serde_json::json!({
                "upload": true,
                "rename": false,
                "delete": true,
            }),
            &admin_cookie,
        )?)
        .await
        .context("send permission edit request")?;
    assert_eq!(edited.status(), StatusCode::OK);
    let edited = response_json(edited).await?;
    assert_eq!(
        edited.pointer("/permissions/upload"),
        Some(&serde_json::Value::Bool(true))
    );
    assert_eq!(
        edited.pointer("/permissions/rename"),
        Some(&serde_json::Value::Bool(false))
    );
    assert_eq!(
        edited.pointer("/permissions/delete"),
        Some(&serde_json::Value::Bool(true))
    );

    let edited_identity = identity_with_cookie(app, &user_cookie).await?;
    assert_eq!(
        edited_identity.pointer("/actions/upload"),
        Some(&serde_json::Value::Bool(true))
    );
    assert_eq!(
        edited_identity.pointer("/actions/rename"),
        Some(&serde_json::Value::Bool(false))
    );
    assert_eq!(
        edited_identity.pointer("/actions/delete"),
        Some(&serde_json::Value::Bool(true))
    );
    Ok(())
}

#[tokio::test]
async fn test_should_edit_anonymous_permissions_without_logged_in_users_inheriting_them()
-> Result<()> {
    let storage_root = tempfile::tempdir().context("create temporary storage root")?;
    let config = config_from_storage_root(storage_root.path()).await?;
    let built = build_router_with_bootstrap_report(config)
        .await
        .context("build router with bootstrap report")?;
    let password = built
        .bootstrap_password()
        .context("first startup should report bootstrap password")?
        .plaintext_password()
        .to_owned();
    let app = built.into_router();
    let admin_cookie = login_session_cookie(app.clone(), "admin", &password).await?;

    let default_anonymous = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/console/anonymous-permissions")
                .header(header::COOKIE, &admin_cookie)
                .body(Body::empty())
                .context("build get anonymous permissions request")?,
        )
        .await
        .context("send get anonymous permissions request")?;
    assert_eq!(default_anonymous.status(), StatusCode::OK);
    let default_anonymous = response_json(default_anonymous).await?;
    assert_eq!(
        default_anonymous.pointer("/upload"),
        Some(&serde_json::Value::Bool(false))
    );
    assert_eq!(
        default_anonymous.pointer("/rename"),
        Some(&serde_json::Value::Bool(false))
    );
    assert_eq!(
        default_anonymous.pointer("/delete"),
        Some(&serde_json::Value::Bool(false))
    );

    let create_user = app
        .clone()
        .oneshot(json_request_with_cookie(
            "POST",
            "/api/console/users",
            serde_json::json!({
                "username": "non-inheriting-user",
                "password": "non-inherit-password",
                "permissions": {
                    "upload": false,
                    "rename": false,
                    "delete": false,
                },
            }),
            &admin_cookie,
        )?)
        .await
        .context("send no-permission user create request")?;
    assert_eq!(create_user.status(), StatusCode::CREATED);

    let edited_anonymous = app
        .clone()
        .oneshot(json_request_with_cookie(
            "PATCH",
            "/api/console/anonymous-permissions",
            serde_json::json!({
                "upload": true,
                "rename": true,
                "delete": false,
            }),
            &admin_cookie,
        )?)
        .await
        .context("send anonymous permission edit request")?;
    assert_eq!(edited_anonymous.status(), StatusCode::OK);

    let anonymous_identity = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/identity")
                .body(Body::empty())
                .context("build anonymous identity request")?,
        )
        .await
        .context("send anonymous identity request")?;
    assert_eq!(anonymous_identity.status(), StatusCode::OK);
    let anonymous_identity = response_json(anonymous_identity).await?;
    assert_eq!(
        anonymous_identity.pointer("/actions/upload"),
        Some(&serde_json::Value::Bool(true))
    );
    assert_eq!(
        anonymous_identity.pointer("/actions/rename"),
        Some(&serde_json::Value::Bool(true))
    );
    assert_eq!(
        anonymous_identity.pointer("/actions/delete"),
        Some(&serde_json::Value::Bool(false))
    );

    let user_cookie =
        login_session_cookie(app.clone(), "non-inheriting-user", "non-inherit-password").await?;
    let user_identity = identity_with_cookie(app, &user_cookie).await?;
    assert_eq!(
        user_identity.pointer("/actions/upload"),
        Some(&serde_json::Value::Bool(false))
    );
    assert_eq!(
        user_identity.pointer("/actions/rename"),
        Some(&serde_json::Value::Bool(false))
    );
    assert_eq!(
        user_identity.pointer("/actions/delete"),
        Some(&serde_json::Value::Bool(false))
    );
    Ok(())
}

#[tokio::test]
async fn test_should_revoke_user_sessions_when_admin_resets_password_or_deletes_user() -> Result<()>
{
    let storage_root = tempfile::tempdir().context("create temporary storage root")?;
    let config = config_from_storage_root(storage_root.path()).await?;
    let built = build_router_with_bootstrap_report(config)
        .await
        .context("build router with bootstrap report")?;
    let password = built
        .bootstrap_password()
        .context("first startup should report bootstrap password")?
        .plaintext_password()
        .to_owned();
    let app = built.into_router();
    let admin_cookie = login_session_cookie(app.clone(), "admin", &password).await?;

    let create_user = app
        .clone()
        .oneshot(json_request_with_cookie(
            "POST",
            "/api/console/users",
            serde_json::json!({
                "username": "reset-delete-user",
                "password": "original-password",
                "permissions": {
                    "upload": true,
                    "rename": false,
                    "delete": false,
                },
            }),
            &admin_cookie,
        )?)
        .await
        .context("send reset/delete user create request")?;
    assert_eq!(create_user.status(), StatusCode::CREATED);

    let old_cookie = login_session_cookie(app.clone(), "reset-delete-user", "original-password")
        .await
        .context("login before reset")?;
    let reset = app
        .clone()
        .oneshot(json_request_with_cookie(
            "POST",
            "/api/console/users/reset-delete-user/password",
            serde_json::json!({
                "password": "replacement-password",
            }),
            &admin_cookie,
        )?)
        .await
        .context("send password reset request")?;
    assert_eq!(reset.status(), StatusCode::NO_CONTENT);

    let old_identity = identity_with_cookie(app.clone(), &old_cookie).await?;
    assert_eq!(
        old_identity.pointer("/authenticated"),
        Some(&serde_json::Value::Bool(false))
    );
    let stale_login = app
        .clone()
        .oneshot(json_request(
            "/api/login",
            serde_json::json!({
                "username": "reset-delete-user",
                "password": "original-password",
            }),
        )?)
        .await
        .context("send stale password login request")?;
    assert_eq!(stale_login.status(), StatusCode::UNAUTHORIZED);

    let new_cookie =
        login_session_cookie(app.clone(), "reset-delete-user", "replacement-password").await?;
    let delete_user = console_request(
        "DELETE",
        "/api/console/users/reset-delete-user",
        &admin_cookie,
    )?;
    let delete_user = app
        .clone()
        .oneshot(delete_user)
        .await
        .context("send delete user request")?;
    assert_eq!(delete_user.status(), StatusCode::NO_CONTENT);

    let deleted_identity = identity_with_cookie(app.clone(), &new_cookie).await?;
    assert_eq!(
        deleted_identity.pointer("/authenticated"),
        Some(&serde_json::Value::Bool(false))
    );
    let deleted_login = app
        .oneshot(json_request(
            "/api/login",
            serde_json::json!({
                "username": "reset-delete-user",
                "password": "replacement-password",
            }),
        )?)
        .await
        .context("send deleted user login request")?;
    assert_eq!(deleted_login.status(), StatusCode::UNAUTHORIZED);
    Ok(())
}

async fn config_from_storage_root(storage_root: &Path) -> Result<AppConfig> {
    let config_dir = TempDir::new().context("create temporary config directory")?;
    let database_path = config_dir.path().join("file-hub.sqlite");
    let config_path = config_dir.path().join("file-hub.yaml");
    let config = format!(
        r#"
storage_root: {storage_root:?}
database_path: {database_path:?}
staging_directory_name: ".fh-staging"
server:
  bind_address: "127.0.0.1:0"
  time_zone: "UTC"
limits:
  listing_direct_child_limit: 10
  archive_resource_count_limit: 100
  archive_uncompressed_size_limit_bytes: 1048576
  search_result_limit: 100
  search_traversal_limit: 1000
  request_timeout_seconds: 5
  fs_concurrency_limit: 4
"#,
        storage_root = storage_root.to_string_lossy(),
        database_path = database_path.to_string_lossy(),
    );
    fs::write(&config_path, config)
        .await
        .context("write temporary config file")?;

    AppConfig::load_from_path(&config_path)
        .await
        .context("load app config")
}

fn json_request(uri: &str, body: serde_json::Value) -> Result<Request<Body>> {
    Request::builder()
        .method("POST")
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .context("build JSON request")
}

fn json_request_with_cookie(
    method: &str,
    uri: &str,
    body: serde_json::Value,
    session_cookie: &str,
) -> Result<Request<Body>> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::COOKIE, session_cookie)
        .body(Body::from(body.to_string()))
        .context("build authenticated JSON request")
}

fn console_request(method: &str, uri: &str, session_cookie: &str) -> Result<Request<Body>> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header(header::COOKIE, session_cookie)
        .body(Body::empty())
        .context("build console request")
}

async fn response_json(response: axum::response::Response) -> Result<serde_json::Value> {
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .context("read response body")?;
    serde_json::from_slice(&body).context("parse response JSON")
}

async fn stored_session_token_hash(database_path: &Path) -> Result<String> {
    let options = SqliteConnectOptions::new().filename(database_path);
    let pool = SqlitePool::connect_with(options)
        .await
        .context("connect to auth SQLite database")?;
    let (token_hash,) = sqlx::query_as::<_, (String,)>("SELECT token_hash FROM sessions")
        .fetch_one(&pool)
        .await
        .context("read stored session token hash")?;
    Ok(token_hash)
}

fn session_token_from_cookie(cookie: &str) -> Result<&str> {
    cookie
        .split(';')
        .find_map(|segment| segment.trim().strip_prefix("fh_session="))
        .context("session cookie should include token")
}

async fn assert_error(
    response: axum::response::Response,
    status: StatusCode,
    code: &str,
) -> Result<()> {
    assert_eq!(response.status(), status);
    let body = response_json(response).await?;
    assert_eq!(
        body.pointer("/error/code"),
        Some(&serde_json::Value::String(code.to_owned()))
    );
    Ok(())
}

async fn login_session_cookie(app: axum::Router, username: &str, password: &str) -> Result<String> {
    let login = app
        .oneshot(json_request(
            "/api/login",
            serde_json::json!({
                "username": username,
                "password": password,
            }),
        )?)
        .await
        .context("send login request")?;
    assert_eq!(login.status(), StatusCode::NO_CONTENT);
    Ok(login
        .headers()
        .get(header::SET_COOKIE)
        .context("login should set session cookie")?
        .to_str()
        .context("session cookie should be ASCII")?
        .to_owned())
}

async fn identity_with_cookie(
    app: axum::Router,
    session_cookie: &str,
) -> Result<serde_json::Value> {
    let identity = app
        .oneshot(
            Request::builder()
                .uri("/api/identity")
                .header(header::COOKIE, session_cookie)
                .body(Body::empty())
                .context("build identity request")?,
        )
        .await
        .context("send identity request")?;
    assert_eq!(identity.status(), StatusCode::OK);
    response_json(identity).await
}
