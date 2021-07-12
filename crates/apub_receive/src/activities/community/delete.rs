use crate::activities::{community::send_websocket_message, verify_mod_action};
use activitystreams::activity::kind::DeleteType;
use lemmy_api_common::blocking;
use lemmy_apub::{
  check_is_apub_id_valid,
  fetcher::{community::get_or_fetch_and_upsert_community, person::get_or_fetch_and_upsert_person},
  ActorType,
  CommunityType,
};
use lemmy_apub_lib::{verify_domains_match, ActivityCommonFields, ActivityHandler, PublicUrl};
use lemmy_db_queries::{source::community::Community_, ApubObject};
use lemmy_db_schema::source::community::Community;
use lemmy_utils::LemmyError;
use lemmy_websocket::{LemmyContext, UserOperationCrud};
use url::Url;

// We have two possibilities which need to be handled:
//     1. actor is remote mod, community id in object
//     2. actor is community, cc is followers collection
#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeleteCommunity {
  to: PublicUrl,
  pub(in crate::activities::community) object: Url,
  cc: [Url; 1],
  #[serde(rename = "type")]
  kind: DeleteType,
  #[serde(flatten)]
  common: ActivityCommonFields,
}

#[async_trait::async_trait(?Send)]
impl ActivityHandler for DeleteCommunity {
  async fn verify(&self, context: &LemmyContext, _: &mut i32) -> Result<(), LemmyError> {
    verify_domains_match(&self.common.actor, self.common.id_unchecked())?;
    let object = self.object.clone();
    let community = blocking(context.pool(), move |conn| {
      Community::read_from_apub_id(conn, &object.into())
    })
    .await?;
    // remote mod action on local community
    if let Ok(c) = community {
      verify_domains_match(&self.object, &self.cc[0])?;
      check_is_apub_id_valid(&self.common.actor, false)?;
      verify_mod_action(&self.common.actor, c.actor_id(), context).await
    }
    // community action sent to followers
    else {
      verify_domains_match(&self.common.actor, &self.object)?;
      verify_domains_match(&self.common.actor, &self.cc[0])
    }
  }

  async fn receive(
    &self,
    context: &LemmyContext,
    request_counter: &mut i32,
  ) -> Result<(), LemmyError> {
    // TODO: match on actor to decide what to do
    let actor = self.object.clone();
    let community = blocking(context.pool(), move |conn| {
      Community::read_from_apub_id(conn, &actor.into())
    })
    .await?;
    let community_id = match community {
      Ok(c) => {
        // remote mod sent delete to local community, forward it to followers
        let actor =
          get_or_fetch_and_upsert_person(&self.common.actor, context, request_counter).await?;
        c.send_delete(actor, context).await?;
        c.id
      }
      Err(_) => {
        // refetch the remote community
        let community =
          get_or_fetch_and_upsert_community(&self.object, context, request_counter).await?;
        community.id
      }
    };
    let deleted_community = blocking(context.pool(), move |conn| {
      Community::update_deleted(conn, community_id, true)
    })
    .await??;

    send_websocket_message(
      deleted_community.id,
      UserOperationCrud::DeleteCommunity,
      context,
    )
    .await
  }

  fn common(&self) -> &ActivityCommonFields {
    &self.common
  }
}
