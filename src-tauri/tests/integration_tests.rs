use flowbit_lib::functions::cache::{playlist_key, video_key};
use flowbit_lib::functions::playlist::is_playlist_url;
use flowbit_lib::functions::valid::{is_twitch_url, is_youtube_url, validate_time_range};
use flowbit_lib::functions::youtube::{
    decode_output, merge_container, parse_time_to_secs, quality_to_format, section_changed,
    Quality,
};

// ═══════════════════════════════════════════════════════════════════════════════
// cache key normalization
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn video_key_ignores_tracking_params() {
    // Same video, shared-link tracking param differs — must hit the same key.
    assert_eq!(
        video_key("https://www.youtube.com/watch?v=abc123&si=xyz"),
        video_key("https://www.youtube.com/watch?v=abc123&si=other")
    );
}

#[test]
fn video_key_ignores_timestamp_and_list_params() {
    assert_eq!(
        video_key("https://www.youtube.com/watch?v=abc123&t=42s&list=PLfoo"),
        video_key("https://www.youtube.com/watch?v=abc123")
    );
}

#[test]
fn video_key_distinguishes_different_videos() {
    assert_ne!(
        video_key("https://www.youtube.com/watch?v=abc123"),
        video_key("https://www.youtube.com/watch?v=def456")
    );
}

#[test]
fn playlist_key_ignores_index_param_keeps_list() {
    assert_eq!(
        playlist_key("https://www.youtube.com/playlist?list=PLfoo&index=3"),
        playlist_key("https://www.youtube.com/playlist?list=PLfoo")
    );
}

#[test]
fn playlist_key_distinguishes_different_playlists() {
    assert_ne!(
        playlist_key("https://www.youtube.com/playlist?list=PLfoo"),
        playlist_key("https://www.youtube.com/playlist?list=PLbar")
    );
}

#[test]
fn video_key_falls_back_to_trimmed_url_when_unparseable() {
    assert_eq!(video_key("  not a url  "), "not a url".to_string());
}

// ═══════════════════════════════════════════════════════════════════════════════
// is_playlist_url
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn playlist_url_youtube_with_list_param() {
    assert!(is_playlist_url("https://youtube.com/playlist?list=PLtest".to_string()));
    assert!(is_playlist_url("https://www.youtube.com/playlist?list=PLtest".to_string()));
}

#[test]
fn playlist_url_youtube_without_list_param_is_false() {
    // BUG: /playlist without list= still matches as a playlist
    assert!(!is_playlist_url("https://youtube.com/playlist".to_string()));
    assert!(!is_playlist_url("https://www.youtube.com/playlist".to_string()));
}

#[test]
fn playlist_url_single_video_with_list_param_is_false() {
    // watch?v= + list= is a radio/queue, not a playlist
    assert!(!is_playlist_url(
        "https://youtube.com/watch?v=VIDEO_ID&list=PLtest".to_string()
    ));
    assert!(!is_playlist_url("https://youtu.be/VIDEO_ID".to_string()));
    assert!(!is_playlist_url("https://youtube.com/shorts/VIDEO_ID".to_string()));
}

#[test]
fn playlist_url_twitch_videos_page() {
    // twitch.tv/channel/videos — channel videos page, counted as a playlist
    assert!(is_playlist_url("https://twitch.tv/somechannel/videos".to_string()));
    assert!(is_playlist_url("https://www.twitch.tv/somechannel/videos".to_string()));
}

#[test]
fn playlist_url_twitch_clips_not_playlist() {
    assert!(!is_playlist_url("https://twitch.tv/clips".to_string()));
}

#[test]
fn playlist_url_twitch_single_video_is_false() {
    // BUG: twitch.tv/videos/12345 matches via contains("/videos")
    assert!(!is_playlist_url("https://twitch.tv/videos/12345".to_string()));
    assert!(!is_playlist_url("https://twitch.tv/channel/v/12345".to_string()));
    assert!(!is_playlist_url("https://clips.twitch.tv/someclip".to_string()));
}

#[test]
fn playlist_url_non_playlist_urls() {
    assert!(!is_playlist_url("https://google.com".to_string()));
    assert!(!is_playlist_url("https://example.com/playlist".to_string()));
    assert!(!is_playlist_url("".to_string()));
}

// ═══════════════════════════════════════════════════════════════════════════════
// validate_time_range
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn time_range_ok_defaults() {
    assert!(validate_time_range(None, None, None).is_ok());
    assert!(validate_time_range(
        Some("00:00:00".into()),
        None,
        None
    )
    .is_ok());
}

#[test]
fn time_range_ok_valid_range() {
    assert!(validate_time_range(
        Some("00:00:00".into()),
        Some("00:01:00".into()),
        Some(120)
    )
    .is_ok());
}

#[test]
fn time_range_err_end_exceeds_duration() {
    assert!(validate_time_range(
        Some("00:00:00".into()),
        Some("00:03:00".into()),
        Some(120)
    )
    .is_err());
}

#[test]
fn time_range_err_start_equals_end() {
    assert!(validate_time_range(
        Some("00:01:00".into()),
        Some("00:01:00".into()),
        Some(300)
    )
    .is_err());
}

#[test]
fn time_range_err_start_after_end() {
    assert!(validate_time_range(
        Some("00:02:00".into()),
        Some("00:01:00".into()),
        Some(300)
    )
    .is_err());
}

#[test]
fn time_range_err_start_exceeds_duration_with_default_end() {
    // BUG: start=60s > duration=30s, end=00:00:00 → passes validation
    assert!(validate_time_range(
        Some("00:01:00".into()),
        Some("00:00:00".into()),
        Some(30)
    )
    .is_err());
}

#[test]
fn time_range_err_invalid_format() {
    assert!(validate_time_range(
        Some("abc".into()),
        Some("00:01:00".into()),
        None
    )
    .is_err());
}

#[test]
fn time_range_err_minutes_or_seconds_ge_60() {
    assert!(validate_time_range(
        Some("00:60:00".into()),
        Some("00:61:00".into()),
        None
    )
    .is_err());
}

// ═══════════════════════════════════════════════════════════════════════════════
// is_youtube_url / is_twitch_url
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn youtube_url_various_formats() {
    assert!(is_youtube_url("https://youtube.com/watch?v=test".to_string()));
    assert!(is_youtube_url("https://www.youtube.com/watch?v=test".to_string()));
    assert!(is_youtube_url("https://youtu.be/test".to_string()));
    assert!(is_youtube_url("https://youtube.com/shorts/test".to_string()));
    assert!(is_youtube_url("https://youtube.com/embed/test".to_string()));
    assert!(is_youtube_url("https://youtube.com/live/test".to_string()));
}

#[test]
fn youtube_url_rejects_invalid() {
    assert!(!is_youtube_url("https://vimeo.com/test".to_string()));
    assert!(!is_youtube_url(
        "https://example.com/youtube.com/watch?v=test".to_string()
    ));
}

#[test]
fn twitch_url_various_formats() {
    // Regex matches only specific patterns: videos/ID, channel/v/ID, clips/
    assert!(is_twitch_url("https://twitch.tv/videos/12345".to_string()));
    assert!(is_twitch_url("https://twitch.tv/channel/v/12345".to_string()));
    assert!(is_twitch_url("https://clips.twitch.tv/someclip".to_string()));
}

#[test]
fn twitch_url_rejects_channel_home() {
    // Channel homepage does NOT match the regex — a regex limitation
    assert!(!is_twitch_url("https://twitch.tv/somechannel".to_string()));
}

#[test]
fn twitch_url_trims_whitespace() {
    // is_twitch_url trims, is_youtube_url does not
    assert!(is_twitch_url("  https://twitch.tv/videos/12345  ".to_string()));
}

#[test]
fn youtube_url_does_not_trim() {
    // BUG: is_youtube_url doesn't trim, is_twitch_url does
    assert!(!is_youtube_url("  https://youtube.com/watch?v=test  ".to_string()));
}

// ═══════════════════════════════════════════════════════════════════════════════
// parse_time_to_secs
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn time_secs_basic() {
    assert_eq!(parse_time_to_secs("00:00:00"), Some(0));
    assert_eq!(parse_time_to_secs("00:01:00"), Some(60));
    assert_eq!(parse_time_to_secs("01:00:00"), Some(3600));
    assert_eq!(parse_time_to_secs("01:02:03"), Some(3723));
}

#[test]
fn time_secs_rejects_invalid() {
    assert!(parse_time_to_secs("").is_none());
    assert!(parse_time_to_secs("abc").is_none());
    assert!(parse_time_to_secs("00:00").is_none());
    // "0:00:00" — 3 parts, parses fine (no zero-padding required)
    assert_eq!(parse_time_to_secs("0:00:00"), Some(0));
    assert!(parse_time_to_secs("00:00:00:00").is_none());
    assert!(parse_time_to_secs("00:60:00").is_none());
    assert!(parse_time_to_secs("00:00:60").is_none());
}

// ═══════════════════════════════════════════════════════════════════════════════
// section_changed
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn section_changed_no_trim_needed() {
    // Both defaults → no trim needed
    assert!(!section_changed("00:00:00", "00:00:00", Some(120)));
    // end >= duration → no trim needed (video shorter than requested end)
    assert!(!section_changed("00:00:00", "02:00:00", Some(120)));
}

#[test]
fn section_changed_trim_needed() {
    // start > 0 → trim needed
    assert!(section_changed("00:01:00", "00:00:00", Some(120)));
    // end < duration → trim needed (150s < 299s)
    assert!(section_changed("00:00:00", "00:02:30", Some(300)));
}

#[test]
fn section_changed_start_exceeds_duration() {
    // BUG: start > duration → true, ffmpeg will fail with an empty file
    let result = section_changed("00:01:00", "00:00:00", Some(30));
    assert!(result); // documented bug
}

#[test]
fn section_changed_unparseable_end_returns_true() {
    // BUG: invalid end → true, an unnecessary ffmpeg call
    let result = section_changed("00:00:00", "invalid", Some(120));
    assert!(result); // documented bug
}

// ═══════════════════════════════════════════════════════════════════════════════
// decode_output
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn decode_output_utf8() {
    assert_eq!(decode_output(b"Hello world"), "Hello world");
    assert_eq!(decode_output("Привет.mp4".as_bytes()), "Привет.mp4");
}

#[test]
fn decode_output_cp1251_fallback() {
    // "Привет" (Cyrillic "hello") in cp1251 — not valid UTF-8
    let cp1251 = [0xCF, 0xF0, 0xE8, 0xE2, 0xE5, 0xF2];
    assert_eq!(decode_output(&cp1251), "Привет");
}

// ═══════════════════════════════════════════════════════════════════════════════
// quality_to_format
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn quality_to_format_variants() {
    let best = quality_to_format(Quality::Best);
    let high = quality_to_format(Quality::High);
    let medium = quality_to_format(Quality::Medium);
    let low = quality_to_format(Quality::Low);
    let worst = quality_to_format(Quality::Worst);

    assert!(best.contains("+"));
    assert!(high.contains("+"));
    assert!(medium.contains("+"));
    assert!(low.contains("+"));
    assert!(worst.contains("+"));

    assert!(best.contains("avc1"));
    assert!(best.contains("mp4a"));
    assert!(worst.contains("wv*"));
}

// ═══════════════════════════════════════════════════════════════════════════════
// merge_container
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn merge_container_opus_gives_mkv() {
    assert_eq!(merge_container(Some("opus")), "mkv");
}

#[test]
fn merge_container_other_gives_mp4() {
    assert_eq!(merge_container(Some("aac")), "mp4");
    assert_eq!(merge_container(None), "mp4");
}
