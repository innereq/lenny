use crate::{
  api::{
    check_community_ban,
    get_post,
    get_user_from_jwt,
    get_user_from_jwt_opt,
    is_mod_or_admin,
    APIError,
    Perform,
  },
  apub::{ApubLikeableType, ApubObjectType},
  blocking,
  websocket::{
    server::{JoinCommunityRoom, SendComment},
    UserOperation,
  },
  ConnectionId,
  DbPool,
  LemmyContext,
  LemmyError,
};
use actix_web::web::Data;
use lemmy_db::{
  comment::*,
  comment_view::*,
  moderator::*,
  post::*,
  site_view::*,
  user::*,
  user_mention::*,
  Crud,
  Likeable,
  ListingType,
  Saveable,
  SortType,
};
use lemmy_utils::{
  make_apub_endpoint,
  scrape_text_for_mentions,
  send_email,
  settings::Settings,
  EndpointType,
  MentionData,
  fake_remove_slurs,
};
use log::error;
use serde::{Deserialize, Serialize};
use std::str::FromStr;

#[derive(Serialize, Deserialize)]
pub struct CreateComment {
  content: String,
  parent_id: Option<i32>,
  pub post_id: i32,
  form_id: Option<String>,
  auth: String,
}

#[derive(Serialize, Deserialize)]
pub struct EditComment {
  content: String,
  edit_id: i32,
  form_id: Option<String>,
  auth: String,
}

#[derive(Serialize, Deserialize)]
pub struct DeleteComment {
  edit_id: i32,
  deleted: bool,
  auth: String,
}

#[derive(Serialize, Deserialize)]
pub struct RemoveComment {
  edit_id: i32,
  removed: bool,
  reason: Option<String>,
  auth: String,
}

#[derive(Serialize, Deserialize)]
pub struct MarkCommentAsRead {
  edit_id: i32,
  read: bool,
  auth: String,
}

#[derive(Serialize, Deserialize)]
pub struct SaveComment {
  comment_id: i32,
  save: bool,
  auth: String,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct CommentResponse {
  pub comment: CommentView,
  pub recipient_ids: Vec<i32>,
  pub form_id: Option<String>,
}

#[derive(Serialize, Deserialize)]
pub struct CreateCommentLike {
  comment_id: i32,
  score: i16,
  auth: String,
}

#[derive(Serialize, Deserialize)]
pub struct GetComments {
  type_: String,
  sort: String,
  page: Option<i64>,
  limit: Option<i64>,
  pub community_id: Option<i32>,
  auth: Option<String>,
}

#[derive(Serialize, Deserialize)]
pub struct GetCommentsResponse {
  comments: Vec<CommentView>,
}

#[async_trait::async_trait(?Send)]
impl Perform for CreateComment {
  type Response = CommentResponse;

  async fn perform(
    &self,
    context: &Data<LemmyContext>,
    websocket_id: Option<ConnectionId>,
  ) -> Result<CommentResponse, LemmyError> {
    let data: &CreateComment = &self;
    let user = get_user_from_jwt(&data.auth, context.pool()).await?;

    // FIXME: Find a way to delete this shit.
    let fake_content_slurs_removed = fake_remove_slurs(&data.content.to_owned());

    let comment_form = CommentForm {
      content: fake_content_slurs_removed,
      parent_id: data.parent_id.to_owned(),
      post_id: data.post_id,
      creator_id: user.id,
      removed: None,
      deleted: None,
      read: None,
      published: None,
      updated: None,
      ap_id: "http://fake.com".into(),
      local: true,
    };

    // Check for a community ban
    let post_id = data.post_id;
    let post = get_post(post_id, context.pool()).await?;

    check_community_ban(user.id, post.community_id, context.pool()).await?;

    // Check if post is locked, no new comments
    if post.locked {
      return Err(APIError::err("locked").into());
    }

    // Create the comment
    let comment_form2 = comment_form.clone();
    let inserted_comment = match blocking(context.pool(), move |conn| {
      Comment::create(&conn, &comment_form2)
    })
    .await?
    {
      Ok(comment) => comment,
      Err(_e) => return Err(APIError::err("couldnt_create_comment").into()),
    };

    // Necessary to update the ap_id
    let inserted_comment_id = inserted_comment.id;
    let updated_comment: Comment = match blocking(context.pool(), move |conn| {
      let apub_id =
        make_apub_endpoint(EndpointType::Comment, &inserted_comment_id.to_string()).to_string();
      Comment::update_ap_id(&conn, inserted_comment_id, apub_id)
    })
    .await?
    {
      Ok(comment) => comment,
      Err(_e) => return Err(APIError::err("couldnt_create_comment").into()),
    };

    updated_comment.send_create(&user, context).await?;

    // Scan the comment for user mentions, add those rows
    let mentions = scrape_text_for_mentions(&comment_form.content);
    let recipient_ids = send_local_notifs(
      mentions,
      updated_comment.clone(),
      &user,
      post,
      context.pool(),
      true,
    )
    .await?;

    // You like your own comment by default
    let like_form = CommentLikeForm {
      comment_id: inserted_comment.id,
      post_id: data.post_id,
      user_id: user.id,
      score: 1,
    };

    let like = move |conn: &'_ _| CommentLike::like(&conn, &like_form);
    if blocking(context.pool(), like).await?.is_err() {
      return Err(APIError::err("couldnt_like_comment").into());
    }

    updated_comment.send_like(&user, context).await?;

    let user_id = user.id;
    let comment_view = blocking(context.pool(), move |conn| {
      CommentView::read(&conn, inserted_comment.id, Some(user_id))
    })
    .await??;

    let mut res = CommentResponse {
      comment: comment_view,
      recipient_ids,
      form_id: data.form_id.to_owned(),
    };

    context.chat_server().do_send(SendComment {
      op: UserOperation::CreateComment,
      comment: res.clone(),
      websocket_id,
    });

    // strip out the recipient_ids, so that
    // users don't get double notifs
    res.recipient_ids = Vec::new();

    Ok(res)
  }
}

#[async_trait::async_trait(?Send)]
impl Perform for EditComment {
  type Response = CommentResponse;

  async fn perform(
    &self,
    context: &Data<LemmyContext>,
    websocket_id: Option<ConnectionId>,
  ) -> Result<CommentResponse, LemmyError> {
    let data: &EditComment = &self;
    let user = get_user_from_jwt(&data.auth, context.pool()).await?;

    let edit_id = data.edit_id;
    let orig_comment = blocking(context.pool(), move |conn| {
      CommentView::read(&conn, edit_id, None)
    })
    .await??;

    check_community_ban(user.id, orig_comment.community_id, context.pool()).await?;

    // Verify that only the creator can edit
    if user.id != orig_comment.creator_id {
      return Err(APIError::err("no_comment_edit_allowed").into());
    }

    // Do the update
    // FIXME: Find a way to delete this shit.
    let fake_content_slurs_removed = fake_remove_slurs(&data.content.to_owned());
    let edit_id = data.edit_id;
    let updated_comment = match blocking(context.pool(), move |conn| {
      Comment::update_content(conn, edit_id, &fake_content_slurs_removed)
    })
    .await?
    {
      Ok(comment) => comment,
      Err(_e) => return Err(APIError::err("couldnt_update_comment").into()),
    };

    // Send the apub update
    updated_comment.send_update(&user, context).await?;

    // Do the mentions / recipients
    let post_id = orig_comment.post_id;
    let post = get_post(post_id, context.pool()).await?;

    let updated_comment_content = updated_comment.content.to_owned();
    let mentions = scrape_text_for_mentions(&updated_comment_content);
    let recipient_ids = send_local_notifs(
      mentions,
      updated_comment,
      &user,
      post,
      context.pool(),
      false,
    )
    .await?;

    let edit_id = data.edit_id;
    let user_id = user.id;
    let comment_view = blocking(context.pool(), move |conn| {
      CommentView::read(conn, edit_id, Some(user_id))
    })
    .await??;

    let mut res = CommentResponse {
      comment: comment_view,
      recipient_ids,
      form_id: data.form_id.to_owned(),
    };

    context.chat_server().do_send(SendComment {
      op: UserOperation::EditComment,
      comment: res.clone(),
      websocket_id,
    });

    // strip out the recipient_ids, so that
    // users don't get double notifs
    res.recipient_ids = Vec::new();

    Ok(res)
  }
}

#[async_trait::async_trait(?Send)]
impl Perform for DeleteComment {
  type Response = CommentResponse;

  async fn perform(
    &self,
    context: &Data<LemmyContext>,
    websocket_id: Option<ConnectionId>,
  ) -> Result<CommentResponse, LemmyError> {
    let data: &DeleteComment = &self;
    let user = get_user_from_jwt(&data.auth, context.pool()).await?;

    let edit_id = data.edit_id;
    let orig_comment = blocking(context.pool(), move |conn| {
      CommentView::read(&conn, edit_id, None)
    })
    .await??;

    check_community_ban(user.id, orig_comment.community_id, context.pool()).await?;

    // Verify that only the creator can delete
    if user.id != orig_comment.creator_id {
      return Err(APIError::err("no_comment_edit_allowed").into());
    }

    // Do the delete
    let deleted = data.deleted;
    let updated_comment = match blocking(context.pool(), move |conn| {
      Comment::update_deleted(conn, edit_id, deleted)
    })
    .await?
    {
      Ok(comment) => comment,
      Err(_e) => return Err(APIError::err("couldnt_update_comment").into()),
    };

    // Send the apub message
    if deleted {
      updated_comment.send_delete(&user, context).await?;
    } else {
      updated_comment.send_undo_delete(&user, context).await?;
    }

    // Refetch it
    let edit_id = data.edit_id;
    let user_id = user.id;
    let comment_view = blocking(context.pool(), move |conn| {
      CommentView::read(conn, edit_id, Some(user_id))
    })
    .await??;

    // Build the recipients
    let post_id = comment_view.post_id;
    let post = get_post(post_id, context.pool()).await?;
    let mentions = vec![];
    let recipient_ids = send_local_notifs(
      mentions,
      updated_comment,
      &user,
      post,
      context.pool(),
      false,
    )
    .await?;

    let mut res = CommentResponse {
      comment: comment_view,
      recipient_ids,
      form_id: None,
    };

    context.chat_server().do_send(SendComment {
      op: UserOperation::DeleteComment,
      comment: res.clone(),
      websocket_id,
    });

    // strip out the recipient_ids, so that
    // users don't get double notifs
    res.recipient_ids = Vec::new();

    Ok(res)
  }
}

#[async_trait::async_trait(?Send)]
impl Perform for RemoveComment {
  type Response = CommentResponse;

  async fn perform(
    &self,
    context: &Data<LemmyContext>,
    websocket_id: Option<ConnectionId>,
  ) -> Result<CommentResponse, LemmyError> {
    let data: &RemoveComment = &self;
    let user = get_user_from_jwt(&data.auth, context.pool()).await?;

    let edit_id = data.edit_id;
    let orig_comment = blocking(context.pool(), move |conn| {
      CommentView::read(&conn, edit_id, None)
    })
    .await??;

    check_community_ban(user.id, orig_comment.community_id, context.pool()).await?;

    // Verify that only a mod or admin can remove
    is_mod_or_admin(context.pool(), user.id, orig_comment.community_id).await?;

    // Do the remove
    let removed = data.removed;
    let updated_comment = match blocking(context.pool(), move |conn| {
      Comment::update_removed(conn, edit_id, removed)
    })
    .await?
    {
      Ok(comment) => comment,
      Err(_e) => return Err(APIError::err("couldnt_update_comment").into()),
    };

    // Mod tables
    let form = ModRemoveCommentForm {
      mod_user_id: user.id,
      comment_id: data.edit_id,
      removed: Some(removed),
      reason: data.reason.to_owned(),
    };
    blocking(context.pool(), move |conn| {
      ModRemoveComment::create(conn, &form)
    })
    .await??;

    // Send the apub message
    if removed {
      updated_comment.send_remove(&user, context).await?;
    } else {
      updated_comment.send_undo_remove(&user, context).await?;
    }

    // Refetch it
    let edit_id = data.edit_id;
    let user_id = user.id;
    let comment_view = blocking(context.pool(), move |conn| {
      CommentView::read(conn, edit_id, Some(user_id))
    })
    .await??;

    // Build the recipients
    let post_id = comment_view.post_id;
    let post = get_post(post_id, context.pool()).await?;
    let mentions = vec![];
    let recipient_ids = send_local_notifs(
      mentions,
      updated_comment,
      &user,
      post,
      context.pool(),
      false,
    )
    .await?;

    let mut res = CommentResponse {
      comment: comment_view,
      recipient_ids,
      form_id: None,
    };

    context.chat_server().do_send(SendComment {
      op: UserOperation::RemoveComment,
      comment: res.clone(),
      websocket_id,
    });

    // strip out the recipient_ids, so that
    // users don't get double notifs
    res.recipient_ids = Vec::new();

    Ok(res)
  }
}

#[async_trait::async_trait(?Send)]
impl Perform for MarkCommentAsRead {
  type Response = CommentResponse;

  async fn perform(
    &self,
    context: &Data<LemmyContext>,
    _websocket_id: Option<ConnectionId>,
  ) -> Result<CommentResponse, LemmyError> {
    let data: &MarkCommentAsRead = &self;
    let user = get_user_from_jwt(&data.auth, context.pool()).await?;

    let edit_id = data.edit_id;
    let orig_comment = blocking(context.pool(), move |conn| {
      CommentView::read(&conn, edit_id, None)
    })
    .await??;

    check_community_ban(user.id, orig_comment.community_id, context.pool()).await?;

    // Verify that only the recipient can mark as read
    // Needs to fetch the parent comment / post to get the recipient
    let parent_id = orig_comment.parent_id;
    match parent_id {
      Some(pid) => {
        let parent_comment = blocking(context.pool(), move |conn| {
          CommentView::read(&conn, pid, None)
        })
        .await??;
        if user.id != parent_comment.creator_id {
          return Err(APIError::err("no_comment_edit_allowed").into());
        }
      }
      None => {
        let parent_post_id = orig_comment.post_id;
        let parent_post =
          blocking(context.pool(), move |conn| Post::read(conn, parent_post_id)).await??;
        if user.id != parent_post.creator_id {
          return Err(APIError::err("no_comment_edit_allowed").into());
        }
      }
    }

    // Do the mark as read
    let read = data.read;
    match blocking(context.pool(), move |conn| {
      Comment::update_read(conn, edit_id, read)
    })
    .await?
    {
      Ok(comment) => comment,
      Err(_e) => return Err(APIError::err("couldnt_update_comment").into()),
    };

    // Refetch it
    let edit_id = data.edit_id;
    let user_id = user.id;
    let comment_view = blocking(context.pool(), move |conn| {
      CommentView::read(conn, edit_id, Some(user_id))
    })
    .await??;

    let res = CommentResponse {
      comment: comment_view,
      recipient_ids: Vec::new(),
      form_id: None,
    };

    Ok(res)
  }
}

#[async_trait::async_trait(?Send)]
impl Perform for SaveComment {
  type Response = CommentResponse;

  async fn perform(
    &self,
    context: &Data<LemmyContext>,
    _websocket_id: Option<ConnectionId>,
  ) -> Result<CommentResponse, LemmyError> {
    let data: &SaveComment = &self;
    let user = get_user_from_jwt(&data.auth, context.pool()).await?;

    let comment_saved_form = CommentSavedForm {
      comment_id: data.comment_id,
      user_id: user.id,
    };

    if data.save {
      let save_comment = move |conn: &'_ _| CommentSaved::save(conn, &comment_saved_form);
      if blocking(context.pool(), save_comment).await?.is_err() {
        return Err(APIError::err("couldnt_save_comment").into());
      }
    } else {
      let unsave_comment = move |conn: &'_ _| CommentSaved::unsave(conn, &comment_saved_form);
      if blocking(context.pool(), unsave_comment).await?.is_err() {
        return Err(APIError::err("couldnt_save_comment").into());
      }
    }

    let comment_id = data.comment_id;
    let user_id = user.id;
    let comment_view = blocking(context.pool(), move |conn| {
      CommentView::read(conn, comment_id, Some(user_id))
    })
    .await??;

    Ok(CommentResponse {
      comment: comment_view,
      recipient_ids: Vec::new(),
      form_id: None,
    })
  }
}

#[async_trait::async_trait(?Send)]
impl Perform for CreateCommentLike {
  type Response = CommentResponse;

  async fn perform(
    &self,
    context: &Data<LemmyContext>,
    websocket_id: Option<ConnectionId>,
  ) -> Result<CommentResponse, LemmyError> {
    let data: &CreateCommentLike = &self;
    let user = get_user_from_jwt(&data.auth, context.pool()).await?;

    let mut recipient_ids = Vec::new();

    // Don't do a downvote if site has downvotes disabled
    if data.score == -1 {
      let site = blocking(context.pool(), move |conn| SiteView::read(conn)).await??;
      if !site.enable_downvotes {
        return Err(APIError::err("downvotes_disabled").into());
      }
    }

    let comment_id = data.comment_id;
    let orig_comment = blocking(context.pool(), move |conn| {
      CommentView::read(&conn, comment_id, None)
    })
    .await??;

    let post_id = orig_comment.post_id;
    let post = get_post(post_id, context.pool()).await?;
    check_community_ban(user.id, post.community_id, context.pool()).await?;

    let comment_id = data.comment_id;
    let comment = blocking(context.pool(), move |conn| Comment::read(conn, comment_id)).await??;

    // Add to recipient ids
    match comment.parent_id {
      Some(parent_id) => {
        let parent_comment =
          blocking(context.pool(), move |conn| Comment::read(conn, parent_id)).await??;
        if parent_comment.creator_id != user.id {
          let parent_user = blocking(context.pool(), move |conn| {
            User_::read(conn, parent_comment.creator_id)
          })
          .await??;
          recipient_ids.push(parent_user.id);
        }
      }
      None => {
        recipient_ids.push(post.creator_id);
      }
    }

    let like_form = CommentLikeForm {
      comment_id: data.comment_id,
      post_id,
      user_id: user.id,
      score: data.score,
    };

    // Remove any likes first
    let user_id = user.id;
    blocking(context.pool(), move |conn| {
      CommentLike::remove(conn, user_id, comment_id)
    })
    .await??;

    // Only add the like if the score isnt 0
    let do_add = like_form.score != 0 && (like_form.score == 1 || like_form.score == -1);
    if do_add {
      let like_form2 = like_form.clone();
      let like = move |conn: &'_ _| CommentLike::like(conn, &like_form2);
      if blocking(context.pool(), like).await?.is_err() {
        return Err(APIError::err("couldnt_like_comment").into());
      }

      if like_form.score == 1 {
        comment.send_like(&user, context).await?;
      } else if like_form.score == -1 {
        comment.send_dislike(&user, context).await?;
      }
    } else {
      comment.send_undo_like(&user, context).await?;
    }

    // Have to refetch the comment to get the current state
    let comment_id = data.comment_id;
    let user_id = user.id;
    let liked_comment = blocking(context.pool(), move |conn| {
      CommentView::read(conn, comment_id, Some(user_id))
    })
    .await??;

    let mut res = CommentResponse {
      comment: liked_comment,
      recipient_ids,
      form_id: None,
    };

    context.chat_server().do_send(SendComment {
      op: UserOperation::CreateCommentLike,
      comment: res.clone(),
      websocket_id,
    });

    // strip out the recipient_ids, so that
    // users don't get double notifs
    res.recipient_ids = Vec::new();

    Ok(res)
  }
}

#[async_trait::async_trait(?Send)]
impl Perform for GetComments {
  type Response = GetCommentsResponse;

  async fn perform(
    &self,
    context: &Data<LemmyContext>,
    websocket_id: Option<ConnectionId>,
  ) -> Result<GetCommentsResponse, LemmyError> {
    let data: &GetComments = &self;
    let user = get_user_from_jwt_opt(&data.auth, context.pool()).await?;
    let user_id = user.map(|u| u.id);

    let type_ = ListingType::from_str(&data.type_)?;
    let sort = SortType::from_str(&data.sort)?;

    let community_id = data.community_id;
    let page = data.page;
    let limit = data.limit;
    let comments = blocking(context.pool(), move |conn| {
      CommentQueryBuilder::create(conn)
        .listing_type(type_)
        .sort(&sort)
        .for_community_id(community_id)
        .my_user_id(user_id)
        .page(page)
        .limit(limit)
        .list()
    })
    .await?;
    let comments = match comments {
      Ok(comments) => comments,
      Err(_) => return Err(APIError::err("couldnt_get_comments").into()),
    };

    if let Some(id) = websocket_id {
      // You don't need to join the specific community room, bc this is already handled by
      // GetCommunity
      if data.community_id.is_none() {
        // 0 is the "all" community
        context.chat_server().do_send(JoinCommunityRoom {
          community_id: 0,
          id,
        });
      }
    }

    Ok(GetCommentsResponse { comments })
  }
}

pub async fn send_local_notifs(
  mentions: Vec<MentionData>,
  comment: Comment,
  user: &User_,
  post: Post,
  pool: &DbPool,
  do_send_email: bool,
) -> Result<Vec<i32>, LemmyError> {
  let user2 = user.clone();
  let ids = blocking(pool, move |conn| {
    do_send_local_notifs(conn, &mentions, &comment, &user2, &post, do_send_email)
  })
  .await?;

  Ok(ids)
}

fn do_send_local_notifs(
  conn: &diesel::PgConnection,
  mentions: &[MentionData],
  comment: &Comment,
  user: &User_,
  post: &Post,
  do_send_email: bool,
) -> Vec<i32> {
  let mut recipient_ids = Vec::new();
  let hostname = &format!("https://{}", Settings::get().hostname);

  // Send the local mentions
  for mention in mentions
    .iter()
    .filter(|m| m.is_local() && m.name.ne(&user.name))
    .collect::<Vec<&MentionData>>()
  {
    if let Ok(mention_user) = User_::read_from_name(&conn, &mention.name) {
      // TODO
      // At some point, make it so you can't tag the parent creator either
      // This can cause two notifications, one for reply and the other for mention
      recipient_ids.push(mention_user.id);

      let user_mention_form = UserMentionForm {
        recipient_id: mention_user.id,
        comment_id: comment.id,
        read: None,
      };

      // Allow this to fail softly, since comment edits might re-update or replace it
      // Let the uniqueness handle this fail
      match UserMention::create(&conn, &user_mention_form) {
        Ok(_mention) => (),
        Err(_e) => error!("{}", &_e),
      };

      // Send an email to those users that have notifications on
      if do_send_email && mention_user.send_notifications_to_email {
        if let Some(mention_email) = mention_user.email {
          let subject = &format!("{} - Mentioned by {}", Settings::get().hostname, user.name,);
          let html = &format!(
            "<h1>User Mention</h1><br><div>{} - {}</div><br><a href={}/inbox>inbox</a>",
            user.name, comment.content, hostname
          );
          match send_email(subject, &mention_email, &mention_user.name, html) {
            Ok(_o) => _o,
            Err(e) => error!("{}", e),
          };
        }
      }
    }
  }

  // Send notifs to the parent commenter / poster
  match comment.parent_id {
    Some(parent_id) => {
      if let Ok(parent_comment) = Comment::read(&conn, parent_id) {
        if parent_comment.creator_id != user.id {
          if let Ok(parent_user) = User_::read(&conn, parent_comment.creator_id) {
            recipient_ids.push(parent_user.id);

            if do_send_email && parent_user.send_notifications_to_email {
              if let Some(comment_reply_email) = parent_user.email {
                let subject = &format!("{} - Reply from {}", Settings::get().hostname, user.name,);
                let html = &format!(
                  "<h1>Comment Reply</h1><br><div>{} - {}</div><br><a href={}/inbox>inbox</a>",
                  user.name, comment.content, hostname
                );
                match send_email(subject, &comment_reply_email, &parent_user.name, html) {
                  Ok(_o) => _o,
                  Err(e) => error!("{}", e),
                };
              }
            }
          }
        }
      }
    }
    // Its a post
    None => {
      if post.creator_id != user.id {
        if let Ok(parent_user) = User_::read(&conn, post.creator_id) {
          recipient_ids.push(parent_user.id);

          if do_send_email && parent_user.send_notifications_to_email {
            if let Some(post_reply_email) = parent_user.email {
              let subject = &format!("{} - Reply from {}", Settings::get().hostname, user.name,);
              let html = &format!(
                "<h1>Post Reply</h1><br><div>{} - {}</div><br><a href={}/inbox>inbox</a>",
                user.name, comment.content, hostname
              );
              match send_email(subject, &post_reply_email, &parent_user.name, html) {
                Ok(_o) => _o,
                Err(e) => error!("{}", e),
              };
            }
          }
        }
      }
    }
  };
  recipient_ids
}
