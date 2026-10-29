use ferrumec::cache::CacheFactory;
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, Pool, Postgres};
use std::sync::Arc;
use uuid::Uuid;
use viewset::{DefaultRepo, DefaultViewSet, Entity, web::DefaultService};

#[derive(Entity, Serialize, Deserialize, Clone, FromRow)]
#[entity(
    table = "notification_tags",
    create = "CreateTag",
    update = "UpdateTag"
)]
pub struct Tag {
    #[entity(skip_create, skip_update)]
    pub id: Uuid,
    #[entity(skip_update)]
    pub tag: String,
    pub user_id: Uuid,
}

#[derive(Serialize, Deserialize)]
pub struct UpdateTag {
    pub user_id: Option<Uuid>,
}

#[derive(Serialize, Deserialize)]
pub struct CreateTag {
    pub tag: String,
    pub user_id: Uuid,
}

type TagRepo = DefaultRepo<Tag>;
type TagService = DefaultService<TagRepo>;
pub type TagViewSet = Arc<DefaultViewSet<TagService>>;

/// Builds the `tags` CRUD viewset. `DefaultRepo` takes its caches from `cf`,
/// the same factory the rest of the module uses.
pub fn create_tag_viewset(db: Pool<Postgres>, cf: impl CacheFactory) -> TagViewSet {
    let repo = TagRepo::new(db, cf);
    Arc::new(DefaultService::from(repo).into())
}
