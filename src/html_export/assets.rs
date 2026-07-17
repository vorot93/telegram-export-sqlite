use crate::error::Result;
use std::{fs, path::Path};

const PNG_1X1_TRANSPARENT: &[u8] = &[
    0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, b'I', b'H', b'D', b'R',
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1F, 0x15, 0xC4,
    0x89, 0x00, 0x00, 0x00, 0x0A, b'I', b'D', b'A', b'T', 0x78, 0x9C, 0x63, 0x00, 0x01, 0x00, 0x00,
    0x05, 0x00, 0x01, 0x0D, 0x0A, 0x2D, 0xB4, 0x00, 0x00, 0x00, 0x00, b'I', b'E', b'N', b'D', 0xAE,
    0x42, 0x60, 0x82,
];

const IMAGE_PATHS: &[&str] = &[
    "images/back.png",
    "images/back@2x.png",
    "images/media_call.png",
    "images/media_call@2x.png",
    "images/media_contact.png",
    "images/media_contact@2x.png",
    "images/media_file.png",
    "images/media_file@2x.png",
    "images/media_game.png",
    "images/media_game@2x.png",
    "images/media_location.png",
    "images/media_location@2x.png",
    "images/media_music.png",
    "images/media_music@2x.png",
    "images/media_photo.png",
    "images/media_photo@2x.png",
    "images/media_shop.png",
    "images/media_shop@2x.png",
    "images/media_video.png",
    "images/media_video@2x.png",
    "images/media_voice.png",
    "images/media_voice@2x.png",
    "images/section_calls.png",
    "images/section_calls@2x.png",
    "images/section_chats.png",
    "images/section_chats@2x.png",
    "images/section_contacts.png",
    "images/section_contacts@2x.png",
    "images/section_frequent.png",
    "images/section_frequent@2x.png",
    "images/section_other.png",
    "images/section_other@2x.png",
    "images/section_photos.png",
    "images/section_photos@2x.png",
    "images/section_sessions.png",
    "images/section_sessions@2x.png",
    "images/section_stories.png",
    "images/section_stories@2x.png",
    "images/section_web.png",
    "images/section_web@2x.png",
];

pub fn write_assets(output_dir: &Path) -> Result<()> {
    fs::create_dir_all(output_dir.join("css"))?;
    fs::create_dir_all(output_dir.join("js"))?;
    fs::create_dir_all(output_dir.join("images"))?;
    fs::write(output_dir.join("css/style.css"), STYLE_CSS)?;
    fs::write(output_dir.join("js/script.js"), SCRIPT_JS)?;
    for path in IMAGE_PATHS {
        fs::write(output_dir.join(path), PNG_1X1_TRANSPARENT)?;
    }
    Ok(())
}

const STYLE_CSS: &str = r#"body{margin:0;background:#fff;color:#222;font:13px/1.35 -apple-system,BlinkMacSystemFont,"Segoe UI",sans-serif}.page_wrap{max-width:920px;margin:0 auto}.page_header{position:sticky;top:0;background:#fff;border-bottom:1px solid #dfe3e8}.page_header .content{padding:14px 18px}.bold{font-weight:700}.history{padding:16px 18px}.message{margin:8px 0}.message.default{clear:both}.message.joined{margin-top:2px}.body{overflow:hidden}.date{float:right;color:#73808c;font-size:12px;margin-left:12px}.from_name{color:#2b6cb0;font-weight:700;margin-bottom:2px}.details{color:#73808c;font-size:12px}.service{text-align:center}.service .body{display:inline-block;background:#eef2f6;border-radius:4px;padding:4px 8px}.text{white-space:normal}.media_wrap{margin:6px 0}.media{display:inline-block;border:1px solid #dfe3e8;border-radius:4px;padding:8px;text-decoration:none;color:inherit}.photo{max-width:520px;height:auto}.thumb{width:42px;height:42px;margin-right:8px}.reply_to{border-left:2px solid #9db7d7;padding-left:6px;margin:4px 0}.forwarded{border-left:2px solid #d0d7de;padding-left:6px}.reactions{display:inline-flex;gap:4px;margin-top:4px}.reaction{border:1px solid #dfe3e8;border-radius:999px;padding:1px 6px}.spoiler.hidden{background:#222;color:#222;cursor:pointer}.bot_buttons_table{border-collapse:collapse;margin-top:6px}.bot_button{border:1px solid #dfe3e8;border-radius:4px;padding:4px 8px}.media_poll{border:1px solid #dfe3e8;border-radius:4px;padding:8px;display:inline-block}.answer{margin-top:4px}"#;

const SCRIPT_JS: &str = r#"function CheckLocation(){return true;}function GoToMessage(id){var e=document.getElementById("message"+id);if(e){e.scrollIntoView();return false;}return true;}function ShowSpoiler(e){e.classList.remove("hidden");}function ShowHashtag(){return false;}function ShowBotCommand(){return false;}function ShowCashtag(){return false;}function ShowMentionName(){return false;}function ShowTextCopied(){return false;}function ShowNotLoadedEmoji(){return false;}function ShowNotAvailableEmoji(){return false;}"#;
