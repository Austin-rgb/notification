use ferrumec::cache::CacheFactory;
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, Pool, Postgres};
use std::sync::Arc;
use uuid::Uuid;
use viewset::{DefaultRepo, DefaultService, DefaultViewSet, Entity};

#[derive(Entity, Serialize, Deserialize, Clone, FromRow)]
#[entity(table = "notification_tags", create = "CreateTag")]
pub struct Tag {
    #[entity(skip_create)]
    pub id: Uuid,
    pub tag: String,
    pub user_id: Uuid,
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
