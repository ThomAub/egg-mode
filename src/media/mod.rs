// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.

//! Functionality to upload images, GIFs, and videos that can be attached to tweets.
//!
//! Tweet media is uploaded separately from the act of posting the tweet itself. In order to attach
//! an image to a new tweet, you need to upload it first, then take the Media ID that Twitter
//! generates and reference that when posting the tweet.
//! The media id is returned as part of the result of a call to [`upload_media`].
//!
//! Here's a basic example of uploading an image and attaching to a tweet:
//!
//! ```rust,no_run
//! # use egg_mode::Token;
//! # #[tokio::main]
//! # async fn main() {
//! # let token: Token = unimplemented!();
//! use egg_mode::media::upload_media;
//! use egg_mode::tweet::DraftTweet;
//!
//! let image = vec![]; //pretend we loaded an image file into this
//! let handle = upload_media(image, media_types::image_png()).await?;
//! let draft = DraftTweet::new("Hey, check out this cute cat!")
//!                        .media_ids(&[media_handle.id]);
//! let tweet = draft.send(&token).await?
//! # }
//! ```
//!
//! For more information, see the [`UploadBuilder`] documentation.

use std::time::{Duration, Instant};

use base64;
use serde::de::Error;
use serde::{Deserialize, Deserializer};

use crate::common::*;
use crate::{auth, error, links};

use mime;

/// A collection of convenience functions that return media types accepted by Twitter.
///
/// These are convenience types that can be handed to [`upload_media`] to set the right media
/// type of a piece of media. The functions in the module correspond to media types that Twitter is
/// known to accept.
///
/// Note that using `image_gif` and `video_mp4` will automatically set the upload's
/// `media_category` to `tweet_gif` and `tweet_video` respectively, allowing larger file sizes and
/// extra processing time.
pub mod media_types {
    use mime::{self, Mime};

    /// PNG images.
    pub fn image_png() -> Mime {
        mime::IMAGE_PNG
    }

    /// JPG images.
    pub fn image_jpg() -> Mime {
        mime::IMAGE_JPEG
    }

    /// WEBP images.
    pub fn image_webp() -> Mime {
        "image/webp".parse().unwrap()
    }

    /// Animated GIF images.
    pub fn image_gif() -> Mime {
        mime::IMAGE_GIF
    }

    /// MP4 videos.
    pub fn video_mp4() -> Mime {
        "video/mp4".parse().unwrap()
    }
}

///RawMedia's upload progressing info.
#[derive(Debug, Clone, PartialEq)]
pub enum ProgressInfo {
    /// Video is pending for processing. Contains number of seconds after which to check.
    Pending(u64),
    /// Video is beeing processed. Contains number of seconds after which to check.
    InProgress(u64),
    /// Video's processing failed. Contains reason.
    Failed(error::MediaError),
    /// Video's processing is finished. RawMedia can be used in other API calls.
    Success,
}

#[derive(Debug, Deserialize)]
enum RawProgressInfoTag {
    #[serde(rename = "pending")]
    Pending,
    #[serde(rename = "in_progress")]
    InProgress,
    #[serde(rename = "failed")]
    Failed,
    #[serde(rename = "succeeded")]
    Success,
}

#[derive(Debug, Deserialize)]
struct RawProgressInfo {
    state: RawProgressInfoTag,
    progress_percent: Option<f64>,
    check_after_secs: Option<u64>,
    error: Option<error::MediaError>,
}

impl<'de> Deserialize<'de> for ProgressInfo {
    fn deserialize<D>(deser: D) -> Result<ProgressInfo, D::Error>
    where
        D: Deserializer<'de>,
    {
        use self::RawProgressInfoTag::*;
        let raw = RawProgressInfo::deserialize(deser)?;
        let check_after = raw
            .check_after_secs
            .ok_or_else(|| D::Error::custom("Missing field: check_after_secs"));
        Ok(match raw.state {
            Pending => ProgressInfo::Pending(check_after?),
            InProgress => ProgressInfo::InProgress(check_after?),
            Success => ProgressInfo::Success,
            Failed => {
                let err = raw
                    .error
                    .ok_or_else(|| D::Error::custom("Missing field: error"))?;
                ProgressInfo::Failed(err)
            }
        })
    }
}

///Represents media file that is uploaded on twitter.
#[derive(Debug, Deserialize)]
pub struct RawMediaHandle {
    ///ID that can be used in API calls (e.g. attach to tweet).
    #[serde(rename = "media_id_string")]
    pub id: String,
    ///Number of second the media can be used in other API calls.
    //We can miss this field on failed upload in which case 0 is pretty reasonable value.
    #[serde(default)]
    #[serde(rename = "expires_after_secs")]
    pub expires_after: u64,
    ///Progress information. If present determines whether RawMedia can be used.
    #[serde(rename = "processing_info")]
    pub progress: Option<ProgressInfo>,
}

#[derive(Debug, Clone, derive_more::From)]
/// An opaque type representing a media id.
pub struct MediaId(pub(crate) String);

/// A handle representing uploaded media.
#[derive(Debug, Clone)]
pub struct MediaHandle {
    pub id: MediaId,
    expires_at: Instant,
    pub progress: Option<ProgressInfo>,
}

impl From<RawMediaHandle> for MediaHandle {
    fn from(raw: RawMediaHandle) -> Self {
        Self {
            id: raw.id.into(),
            // this conversion only makes sense if we create it immediately
            // after receiving from the server!
            expires_at: Instant::now() + Duration::from_secs(raw.expires_after),
            progress: raw.progress,
        }
    }
}

impl MediaHandle {
    /// Media uploads expire after a certain amount of time
    /// This method returns true if the upload is still valid
    /// and can therefore e.g. be attached to a tweet
    pub fn is_valid(&self) -> bool {
        Instant::now() < self.expires_at
    }
}

/// Represents the kinda of media that Twitter will accept.
/// `.to_string()` will return a string suitable for use
#[derive(Debug, Copy, Clone, PartialEq, Eq, derive_more::Display)]
enum MediaCategory {
    /// Static image. Four can be attached to a single tweet.
    #[display(fmt = "tweet_image")]
    Image,
    /// Animated GIF.
    #[display(fmt = "tweet_gif")]
    Gif,
    /// Video.
    #[display(fmt = "tweet_video")]
    Video,
}

impl From<&mime::Mime> for MediaCategory {
    fn from(mime: &mime::Mime) -> Self {
        if mime == &media_types::image_gif() {
            MediaCategory::Gif
        } else if mime == &media_types::video_mp4() {
            MediaCategory::Video
        } else {
            // fallthrough
            MediaCategory::Image
        }
    }
}

/// Upload media to the server.
pub async fn upload_media(
    data: &[u8],
    media_type: &mime::Mime,
    token: &auth::Token,
) -> error::Result<MediaHandle> {
    let media_category = MediaCategory::from(media_type);
    let params = ParamList::new()
        .add_param("command", "INIT")
        .add_param("total_bytes", data.len().to_string())
        .add_param("media_type", media_type.to_string())
        .add_param("media_category", media_category.to_string());
    let req = auth::post(links::media::UPLOAD, &token, Some(&params));
    let media = twitter_json_request::<RawMediaHandle>(req).await?.response;
    let timeout = Instant::now() + Duration::from_secs(media.expires_after);

    let nchunks = data.len() / 1024 * 1024; // divide into 1MB chunks
    for (ix, chunk) in data.chunks(nchunks).enumerate() {
        if Instant::now() > timeout {
            todo!()
        }
        let params = ParamList::new()
            .add_param("command", "APPEND")
            .add_param("media_id", media.id.clone())
            .add_param("media_data", base64::encode(chunk))
            .add_param("segment_index", ix.to_string());
        let req = auth::post(links::media::UPLOAD, token, Some(&params));
        // This request has no response (upon success)
        twitter_raw_request(req).await?;
    }

    let params = ParamList::new()
        .add_param("command", "FINALIZE")
        .add_param("media_id", media.id.clone());
    let req = auth::post(links::media::UPLOAD, token, Some(&params));
    Ok(twitter_json_request::<RawMediaHandle>(req)
        .await?
        .response
        .into())
}

/// Set metadata for a media upload. At the moment the only attribute that may
/// be set is `alt-text`.
pub async fn set_metadata(
    media_id: &MediaId,
    alt_text: &str,
    token: &auth::Token,
) -> error::Result<()> {
    let payload = serde_json::json!({
        "media_id": media_id.0,
        "alt_text": {
            "text": alt_text
        }
    });
    let req = auth::post_json(links::media::METADATA, &token, payload);
    twitter_raw_request(req).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::RawMedia;
    use crate::common::tests::load_file;

    fn load_media(path: &str) -> RawMedia {
        let content = load_file(path);
        ::serde_json::from_str::<RawMedia>(&content).unwrap()
    }

    #[test]
    fn parse_media() {
        let media = load_media("sample_payloads/media.json");

        assert_eq!(media.id, 710511363345354753);
        assert_eq!(media.expires_after, 86400);
    }

    #[test]
    fn parse_media_pending() {
        let media = load_media("sample_payloads/media_pending.json");

        assert_eq!(media.id, 13);
        assert_eq!(media.expires_after, 86400);
        assert!(media.progress.is_some());

        match media.progress {
            Some(super::ProgressInfo::Pending(5)) => (),
            other => assert!(false, format!("Unexpected value of progress={:?}", other)),
        }
    }

    #[test]
    fn parse_media_in_progress() {
        let media = load_media("sample_payloads/media_in_progress.json");

        assert_eq!(media.id, 13);
        assert_eq!(media.expires_after, 3595);
        assert!(media.progress.is_some());

        match media.progress {
            Some(super::ProgressInfo::InProgress(10)) => (),
            other => assert!(false, format!("Unexpected value of progress={:?}", other)),
        }
    }

    #[test]
    fn parse_media_fail() {
        let media = load_media("sample_payloads/media_fail.json");

        assert_eq!(media.id, 710511363345354753);
        assert_eq!(media.expires_after, 0);
        assert!(media.progress.is_some());

        match media.progress {
            Some(super::ProgressInfo::Failed(error)) => assert_eq!(
                error,
                crate::error::MediaError {
                    code: 1,
                    name: "InvalidMedia".to_string(),
                    message: "Unsupported video format".to_string(),
                }
            ),
            other => assert!(false, format!("Unexpected value of progress={:?}", other)),
        }
    }
}
