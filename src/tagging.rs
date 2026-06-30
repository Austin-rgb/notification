use actix_web::{HttpResponse, Responder, delete, get, post, web};
use actixutils::{Auth, Authority, middleware::Pagination};
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use uuid::Uuid;

#[derive(Serialize, Deserialize)]
pub struct TagEntry {
    pub tag: String,
    pub user_id: Uuid,
}

#[post("/tags")]
async fn register_tag(
    pool: web::Data<SqlitePool>,
    req: web::Json<TagEntry>,
    Auth(claims): Auth<Authority>,
) -> impl Responder {
    if !claims.check(16) {
        return HttpResponse::Unauthorized().finish();
    }

    match sqlx::query!(
        r#"
        INSERT INTO notification_tags(tag, user_id)
        VALUES(?, ?)
        ON CONFLICT(tag)
        DO UPDATE SET user_id = excluded.user_id
        "#,
        req.tag,
        req.user_id,
    )
    .execute(pool.get_ref())
    .await
    {
        Ok(_) => HttpResponse::Ok().finish(),
        Err(e) => {
            tracing::error!("error inserting tag: {e}");
            HttpResponse::InternalServerError().finish()
        }
    }
}

#[get("/tags")]
async fn list_tags(pool: web::Data<SqlitePool>, Auth(claims): Auth<Authority>) -> impl Responder {
    if !claims.check(17) {
        return HttpResponse::Unauthorized().finish();
    }

    let pagination = Pagination::get();
    let limit = pagination.limit;
    let offset = pagination.page * limit;
    match sqlx::query_as!(
        TagEntry,
        r#"
        SELECT tag, user_id as "user_id: Uuid"
        FROM notification_tags
        ORDER BY tag
        LIMIT ? OFFSET ?
        "#,
        limit,
        offset
    )
    .fetch_all(pool.get_ref())
    .await
    {
        Ok(tags) => HttpResponse::Ok().json(tags),
        Err(e) => {
            tracing::error!("error listing tags: {e}");
            HttpResponse::InternalServerError().finish()
        }
    }
}

#[delete("/tags/{tag}")]
async fn delete_tag(
    pool: web::Data<SqlitePool>,
    tag: web::Path<String>,
    Auth(claims): Auth<Authority>,
) -> impl Responder {
    if !claims.check(18) {
        return HttpResponse::Unauthorized().finish();
    }
    let tag = tag.into_inner();
    match sqlx::query!("DELETE FROM notification_tags WHERE tag = ?", tag,)
        .execute(pool.get_ref())
        .await
    {
        Ok(result) if result.rows_affected() == 0 => HttpResponse::NotFound().finish(),
        Ok(_) => HttpResponse::NoContent().finish(),
        Err(e) => {
            tracing::error!("error deleting tag: {e}");
            HttpResponse::InternalServerError().finish()
        }
    }
}
