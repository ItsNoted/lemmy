use crate::{
  activities::{
    community::announce::AnnouncableActivities,
    deletion::{
      delete::Delete,
      receive_delete_action,
      verify_delete_activity,
      DeletableObjects,
      WebsocketMessages,
    },
    generate_activity_id,
    verify_activity,
  },
  activity_queue::send_to_community_new,
  extensions::context::lemmy_context,
  ActorType,
};
use activitystreams::activity::kind::{DeleteType, UndoType};
use anyhow::anyhow;
use lemmy_api_common::blocking;
use lemmy_apub_lib::{values::PublicUrl, ActivityCommonFields, ActivityHandler};
use lemmy_db_queries::source::{comment::Comment_, community::Community_, post::Post_};
use lemmy_db_schema::source::{comment::Comment, community::Community, person::Person, post::Post};
use lemmy_utils::LemmyError;
use lemmy_websocket::{
  send::{send_comment_ws_message_simple, send_community_ws_message, send_post_ws_message},
  LemmyContext,
  UserOperationCrud,
};
use url::Url;

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UndoDelete {
  to: PublicUrl,
  object: Delete,
  cc: [Url; 1],
  #[serde(rename = "type")]
  kind: UndoType,
  #[serde(flatten)]
  common: ActivityCommonFields,
}

#[async_trait::async_trait(?Send)]
impl ActivityHandler for UndoDelete {
  async fn verify(
    &self,
    context: &LemmyContext,
    request_counter: &mut i32,
  ) -> Result<(), LemmyError> {
    verify_activity(self.common())?;
    self.object.verify(context, request_counter).await?;
    verify_delete_activity(
      &self.object.object,
      &self.cc[0],
      &self.common,
      self.object.summary.is_some(),
      context,
      request_counter,
    )
    .await?;
    Ok(())
  }

  async fn receive(
    self,
    context: &LemmyContext,
    request_counter: &mut i32,
  ) -> Result<(), LemmyError> {
    if self.object.summary.is_some() {
      UndoDelete::receive_undo_remove_action(&self.object.object, context).await
    } else {
      receive_delete_action(
        &self.object.object,
        &self.common.actor,
        WebsocketMessages {
          community: UserOperationCrud::EditCommunity,
          post: UserOperationCrud::EditPost,
          comment: UserOperationCrud::EditComment,
        },
        false,
        context,
        request_counter,
      )
      .await
    }
  }

  fn common(&self) -> &ActivityCommonFields {
    &self.common
  }
}

impl UndoDelete {
  pub(in crate::activities::deletion) async fn send(
    actor: &Person,
    community: &Community,
    object_id: Url,
    summary: Option<String>,
    context: &LemmyContext,
  ) -> Result<(), LemmyError> {
    let delete = Delete {
      to: PublicUrl::Public,
      object: object_id,
      cc: [community.actor_id()],
      kind: DeleteType::Delete,
      summary,
      common: ActivityCommonFields {
        context: lemmy_context(),
        id: generate_activity_id(DeleteType::Delete)?,
        actor: actor.actor_id(),
        unparsed: Default::default(),
      },
    };

    let id = generate_activity_id(UndoType::Undo)?;
    let undo = UndoDelete {
      to: PublicUrl::Public,
      object: delete,
      cc: [community.actor_id()],
      kind: UndoType::Undo,
      common: ActivityCommonFields {
        context: lemmy_context(),
        id: id.clone(),
        actor: actor.actor_id(),
        unparsed: Default::default(),
      },
    };

    let activity = AnnouncableActivities::UndoDelete(undo);
    send_to_community_new(activity, &id, actor, community, vec![], context).await
  }

  pub(in crate::activities) async fn receive_undo_remove_action(
    object: &Url,
    context: &LemmyContext,
  ) -> Result<(), LemmyError> {
    use UserOperationCrud::*;
    match DeletableObjects::read_from_db(object, context).await? {
      DeletableObjects::Community(community) => {
        if community.local {
          return Err(anyhow!("Only local admin can restore community").into());
        }
        let deleted_community = blocking(context.pool(), move |conn| {
          Community::update_removed(conn, community.id, false)
        })
        .await??;
        send_community_ws_message(deleted_community.id, EditCommunity, None, None, context).await?;
      }
      DeletableObjects::Post(post) => {
        let removed_post = blocking(context.pool(), move |conn| {
          Post::update_removed(conn, post.id, false)
        })
        .await??;
        send_post_ws_message(removed_post.id, EditPost, None, None, context).await?;
      }
      DeletableObjects::Comment(comment) => {
        let removed_comment = blocking(context.pool(), move |conn| {
          Comment::update_removed(conn, comment.id, false)
        })
        .await??;
        send_comment_ws_message_simple(removed_comment.id, EditComment, context).await?;
      }
    }
    Ok(())
  }
}
