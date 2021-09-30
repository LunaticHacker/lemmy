use crate::{
  activities::{
    community::announce::AnnouncableActivities,
    generate_activity_id,
    verify_activity,
    verify_add_remove_moderator_target,
    verify_mod_action,
    verify_person_in_community,
  },
  activity_queue::send_to_community_new,
  extensions::context::lemmy_context,
  fetcher::object_id::ObjectId,
  generate_moderators_url,
  ActorType,
};
use activitystreams::{
  activity::kind::AddType,
  base::AnyBase,
  primitives::OneOrMany,
  unparsed::Unparsed,
};
use lemmy_api_common::blocking;
use lemmy_apub_lib::{values::PublicUrl, ActivityFields, ActivityHandler, Data};
use lemmy_db_queries::{source::community::CommunityModerator_, Joinable};
use lemmy_db_schema::source::{
  community::{Community, CommunityModerator, CommunityModeratorForm},
  person::Person,
};
use lemmy_utils::LemmyError;
use lemmy_websocket::LemmyContext;
use serde::{Deserialize, Serialize};
use url::Url;

#[derive(Clone, Debug, Deserialize, Serialize, ActivityFields)]
#[serde(rename_all = "camelCase")]
pub struct AddMod {
  actor: ObjectId<Person>,
  to: [PublicUrl; 1],
  object: ObjectId<Person>,
  target: Url,
  cc: [ObjectId<Community>; 1],
  #[serde(rename = "type")]
  kind: AddType,
  id: Url,
  #[serde(rename = "@context")]
  context: OneOrMany<AnyBase>,
  #[serde(flatten)]
  unparsed: Unparsed,
}

impl AddMod {
  pub async fn send(
    community: &Community,
    added_mod: &Person,
    actor: &Person,
    context: &LemmyContext,
  ) -> Result<(), LemmyError> {
    let id = generate_activity_id(
      AddType::Add,
      &context.settings().get_protocol_and_hostname(),
    )?;
    let add = AddMod {
      actor: ObjectId::new(actor.actor_id()),
      to: [PublicUrl::Public],
      object: ObjectId::new(added_mod.actor_id()),
      target: generate_moderators_url(&community.actor_id)?.into(),
      cc: [ObjectId::new(community.actor_id())],
      kind: AddType::Add,
      id: id.clone(),
      context: lemmy_context(),
      unparsed: Default::default(),
    };

    let activity = AnnouncableActivities::AddMod(add);
    let inboxes = vec![added_mod.get_shared_inbox_or_inbox_url()];
    send_to_community_new(activity, &id, actor, community, inboxes, context).await
  }
}

#[async_trait::async_trait(?Send)]
impl ActivityHandler for AddMod {
  type DataType = LemmyContext;

  async fn verify(
    &self,
    context: &Data<LemmyContext>,
    request_counter: &mut i32,
  ) -> Result<(), LemmyError> {
    verify_activity(self, &context.settings())?;
    verify_person_in_community(&self.actor, &self.cc[0], context, request_counter).await?;
    verify_mod_action(&self.actor, self.cc[0].clone(), context).await?;
    verify_add_remove_moderator_target(&self.target, &self.cc[0])?;
    Ok(())
  }

  async fn receive(
    self,
    context: &Data<LemmyContext>,
    request_counter: &mut i32,
  ) -> Result<(), LemmyError> {
    let community = self.cc[0].dereference(context, request_counter).await?;
    let new_mod = self.object.dereference(context, request_counter).await?;

    // If we had to refetch the community while parsing the activity, then the new mod has already
    // been added. Skip it here as it would result in a duplicate key error.
    let new_mod_id = new_mod.id;
    let moderated_communities = blocking(context.pool(), move |conn| {
      CommunityModerator::get_person_moderated_communities(conn, new_mod_id)
    })
    .await??;
    if !moderated_communities.contains(&community.id) {
      let form = CommunityModeratorForm {
        community_id: community.id,
        person_id: new_mod.id,
      };
      blocking(context.pool(), move |conn| {
        CommunityModerator::join(conn, &form)
      })
      .await??;
    }
    // TODO: send websocket notification about added mod
    Ok(())
  }
}
