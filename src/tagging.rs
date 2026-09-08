use serde::{Deserialize, Serialize};
use sqlx::{FromRow, Pool, Postgres};
use std::sync::Arc;
use uuid::Uuid;
use viewset::{DefaultRepo, DefaultService, DefaultViewSet, Entity};

#[derive(Entity, Serialize, Deserialize, Clone, FromRow)]
#[entity(create = "CreateTag")]
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
type TagViewSet = Arc<DefaultViewSet<TagService>>;

pub fn create_tag_viewset(db: Pool<Postgres>) -> TagViewSet {
    Arc::new(db.into())
}
