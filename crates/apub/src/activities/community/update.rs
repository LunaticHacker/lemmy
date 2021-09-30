use crate::{
  activities::{
    community::announce::AnnouncableActivities,
    generate_activity_id,
    verify_activity,
    verify_mod_action,
    verify_person_in_community,
  },
  activity_queue::send_to_community_new,
  extensions::context::lemmy_context,
  fetcher::object_id::ObjectId,
  objects::{community::Group, ToApub},
  ActorType,
};
use activitystreams::{
  activity::kind::UpdateType,
  base::AnyBase,
  primitives::OneOrMany,
  unparsed::Unparsed,
};
use lemmy_api_common::blocking;
use lemmy_apub_lib::{values::PublicUrl, ActivityFields, ActivityHandler, Data};
use lemmy_db_queries::{ApubObject, Crud};
use lemmy_db_schema::source::{
  community::{Community, CommunityForm},
  person::Person,
};
use lemmy_utils::LemmyError;
use lemmy_websocket::{send::send_community_ws_message, LemmyContext, UserOperationCrud};
use serde::{Deserialize, Serialize};
use url::Url;

/// This activity is received from a remote community mod, and updates the description or other
/// fields of a local community.
#[derive(Clone, Debug, Deserialize, Serialize, ActivityFields)]
#[serde(rename_all = "camelCase")]
pub struct UpdateCommunity {
  actor: ObjectId<Person>,
  to: [PublicUrl; 1],
  // TODO: would be nice to use a separate struct here, which only contains the fields updated here
  object: Group,
  cc: [ObjectId<Community>; 1],
  #[serde(rename = "type")]
  kind: UpdateType,
  id: Url,
  #[serde(rename = "@context")]
  context: OneOrMany<AnyBase>,
  #[serde(flatten)]
  unparsed: Unparsed,
}

impl UpdateCommunity {
  pub async fn send(
    community: &Community,
    actor: &Person,
    context: &LemmyContext,
  ) -> Result<(), LemmyError> {
    let id = generate_activity_id(
      UpdateType::Update,
      &context.settings().get_protocol_and_hostname(),
    )?;
    let update = UpdateCommunity {
      actor: ObjectId::new(actor.actor_id()),
      to: [PublicUrl::Public],
      object: community.to_apub(context.pool()).await?,
      cc: [ObjectId::new(community.actor_id())],
      kind: UpdateType::Update,
      id: id.clone(),
      context: lemmy_context(),
      unparsed: Default::default(),
    };

    let activity = AnnouncableActivities::UpdateCommunity(Box::new(update));
    send_to_community_new(activity, &id, actor, community, vec![], context).await
  }
}

#[async_trait::async_trait(?Send)]
impl ActivityHandler for UpdateCommunity {
  type DataType = LemmyContext;
  async fn verify(
    &self,
    context: &Data<LemmyContext>,
    request_counter: &mut i32,
  ) -> Result<(), LemmyError> {
    verify_activity(self, &context.settings())?;
    verify_person_in_community(&self.actor, &self.cc[0], context, request_counter).await?;
    verify_mod_action(&self.actor, self.cc[0].clone(), context).await?;
    Ok(())
  }

  async fn receive(
    self,
    context: &Data<LemmyContext>,
    _request_counter: &mut i32,
  ) -> Result<(), LemmyError> {
    let cc = self.cc[0].clone().into();
    let community = blocking(context.pool(), move |conn| {
      Community::read_from_apub_id(conn, &cc)
    })
    .await??;

    let updated_community = Group::from_apub_to_form(
      &self.object,
      &community.actor_id.clone().into(),
      &context.settings(),
    )
    .await?;
    let cf = CommunityForm {
      name: updated_community.name,
      title: updated_community.title,
      description: updated_community.description,
      nsfw: updated_community.nsfw,
      // TODO: icon and banner would be hosted on the other instance, ideally we would copy it to ours
      icon: updated_community.icon,
      banner: updated_community.banner,
      ..CommunityForm::default()
    };
    let updated_community = blocking(context.pool(), move |conn| {
      Community::update(conn, community.id, &cf)
    })
    .await??;

    send_community_ws_message(
      updated_community.id,
      UserOperationCrud::EditCommunity,
      None,
      None,
      context,
    )
    .await?;
    Ok(())
  }
}
