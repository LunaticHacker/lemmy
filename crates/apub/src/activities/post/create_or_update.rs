use crate::{
  activities::{
    community::announce::AnnouncableActivities,
    generate_activity_id,
    verify_activity,
    verify_mod_action,
    verify_person_in_community,
    CreateOrUpdateType,
  },
  activity_queue::send_to_community_new,
  extensions::context::lemmy_context,
  fetcher::object_id::ObjectId,
  objects::{post::Page, FromApub, ToApub},
  ActorType,
};
use activitystreams::{base::AnyBase, primitives::OneOrMany, unparsed::Unparsed};
use anyhow::anyhow;
use lemmy_api_common::blocking;
use lemmy_apub_lib::{
  values::PublicUrl,
  verify_domains_match,
  verify_urls_match,
  ActivityFields,
  ActivityHandler,
  Data,
};
use lemmy_db_queries::Crud;
use lemmy_db_schema::source::{community::Community, person::Person, post::Post};
use lemmy_utils::LemmyError;
use lemmy_websocket::{send::send_post_ws_message, LemmyContext, UserOperationCrud};
use serde::{Deserialize, Serialize};
use url::Url;

#[derive(Clone, Debug, Deserialize, Serialize, ActivityFields)]
#[serde(rename_all = "camelCase")]
pub struct CreateOrUpdatePost {
  actor: ObjectId<Person>,
  to: [PublicUrl; 1],
  object: Page,
  cc: [ObjectId<Community>; 1],
  #[serde(rename = "type")]
  kind: CreateOrUpdateType,
  id: Url,
  #[serde(rename = "@context")]
  context: OneOrMany<AnyBase>,
  #[serde(flatten)]
  unparsed: Unparsed,
}

impl CreateOrUpdatePost {
  pub async fn send(
    post: &Post,
    actor: &Person,
    kind: CreateOrUpdateType,
    context: &LemmyContext,
  ) -> Result<(), LemmyError> {
    let community_id = post.community_id;
    let community = blocking(context.pool(), move |conn| {
      Community::read(conn, community_id)
    })
    .await??;

    let id = generate_activity_id(
      kind.clone(),
      &context.settings().get_protocol_and_hostname(),
    )?;
    let create_or_update = CreateOrUpdatePost {
      actor: ObjectId::new(actor.actor_id()),
      to: [PublicUrl::Public],
      object: post.to_apub(context.pool()).await?,
      cc: [ObjectId::new(community.actor_id())],
      kind,
      id: id.clone(),
      context: lemmy_context(),
      unparsed: Default::default(),
    };

    let activity = AnnouncableActivities::CreateOrUpdatePost(Box::new(create_or_update));
    send_to_community_new(activity, &id, actor, &community, vec![], context).await
  }
}

#[async_trait::async_trait(?Send)]
impl ActivityHandler for CreateOrUpdatePost {
  type DataType = LemmyContext;
  async fn verify(
    &self,
    context: &Data<LemmyContext>,
    request_counter: &mut i32,
  ) -> Result<(), LemmyError> {
    verify_activity(self, &context.settings())?;
    let community = self.cc[0].dereference(context, request_counter).await?;
    verify_person_in_community(&self.actor, &self.cc[0], context, request_counter).await?;
    match self.kind {
      CreateOrUpdateType::Create => {
        verify_domains_match(self.actor.inner(), self.object.id_unchecked())?;
        verify_urls_match(self.actor(), self.object.attributed_to.inner())?;
        // Check that the post isnt locked or stickied, as that isnt possible for newly created posts.
        // However, when fetching a remote post we generate a new create activity with the current
        // locked/stickied value, so this check may fail. So only check if its a local community,
        // because then we will definitely receive all create and update activities separately.
        let is_stickied_or_locked =
          self.object.stickied == Some(true) || self.object.comments_enabled == Some(false);
        if community.local && is_stickied_or_locked {
          return Err(anyhow!("New post cannot be stickied or locked").into());
        }
      }
      CreateOrUpdateType::Update => {
        let is_mod_action = self.object.is_mod_action(context.pool()).await?;
        if is_mod_action {
          verify_mod_action(&self.actor, self.cc[0].clone(), context).await?;
        } else {
          verify_domains_match(self.actor.inner(), self.object.id_unchecked())?;
          verify_urls_match(self.actor(), self.object.attributed_to.inner())?;
        }
      }
    }
    self.object.verify(context, request_counter).await?;
    Ok(())
  }

  async fn receive(
    self,
    context: &Data<LemmyContext>,
    request_counter: &mut i32,
  ) -> Result<(), LemmyError> {
    let actor = self.actor.dereference(context, request_counter).await?;
    let post = Post::from_apub(&self.object, context, &actor.actor_id(), request_counter).await?;

    let notif_type = match self.kind {
      CreateOrUpdateType::Create => UserOperationCrud::CreatePost,
      CreateOrUpdateType::Update => UserOperationCrud::EditPost,
    };
    send_post_ws_message(post.id, notif_type, None, None, context).await?;
    Ok(())
  }
}
