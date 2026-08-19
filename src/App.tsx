import "./App.css";
import {useState, useEffect, useCallback, useRef, useMemo, memo} from "react";
import {invoke} from "@tauri-apps/api/core";
import {openUrl} from "@tauri-apps/plugin-opener";
import {open} from "@tauri-apps/plugin-dialog";
import {listen} from "@tauri-apps/api/event";

type Platform = "youtube" | "twitch" | null;
type Quality = "best" | "high" | "medium" | "low" | "worst";
type DownloadMode = "video" | "audio";
type UrlKind = "single" | "playlist" | null;

interface YoutubeInfo {
    title: string;
    author_name: string;
    html: string;
    duration: number | null;
    audio_tracks: string[];
    video_codecs: string[];
    audio_codecs: string[];
}

interface TwitchInfo {
    title: string;
    channel: string;
    duration: number | null;
    is_live: boolean;
    thumbnail_url: string | null;
    audio_tracks: string[];
    video_codecs: string[];
    audio_codecs: string[];
}

interface DownloadResult {
    path: string;
    file_size_mb: number;
}

interface PlaylistEntry {
    id: string;
    title: string;
    duration: number | null;
    url: string;
}

interface PlaylistInfo {
    title: string;
    uploader: string;
    count: number;
    entries: PlaylistEntry[];
}

interface PlaylistDownloadResult {
    dir: string;
    downloaded: number;
    total: number;
    total_size_mb: number;
}

function formatDuration(s: number) {
    const h = Math.floor(s / 3600);
    const m = Math.floor((s % 3600) / 60);
    const sec = s % 60;
    return `${String(h).padStart(2, "0")}:${String(m).padStart(2, "0")}:${String(sec).padStart(2, "0")}`;
}

// Must match CANCEL_MSG on the backend (youtube.rs).
const CANCEL_MSG = "Download cancelled";

// Language code ("en", "ru", "en-US") -> display name in English ("English", …).
function langName(code: string): string {
    try {
        return new Intl.DisplayNames(["en"], {type: "language"}).of(code) ?? code;
    } catch {
        return code;
    }
}

const QUALITIES: { value: Quality; label: string }[] = [
    {value: "best", label: "Best"},
    {value: "high", label: "1080p"},
    {value: "medium", label: "720p"},
    {value: "low", label: "480p"},
    {value: "worst", label: "Worst"},
];

// Human-readable codec names (short names come from the backend).
const VCODEC_NAMES: Record<string, string> = {h264: "H.264", vp9: "VP9", av1: "AV1"};
const ACODEC_NAMES: Record<string, string> = {aac: "AAC", opus: "Opus"};

const SearchIcon = () => (
    <svg viewBox="0 0 24 24" fill="none" strokeWidth="2" stroke="currentColor">
        <circle cx="11" cy="11" r="8"/>
        <line x1="21" y1="21" x2="16.65" y2="16.65"/>
    </svg>
);

const VideoIcon = () => (
    <svg viewBox="0 0 24 24" fill="none" strokeWidth="1.5" stroke="currentColor">
        <path d="M15 10l4.553-2.07A1 1 0 0121 8.81v6.38a1 1 0 01-1.447.9L15 14"/>
        <rect x="3" y="6" width="12" height="12" rx="2"/>
    </svg>
);

const TerminalIcon = () => (
    <svg viewBox="0 0 24 24" fill="none" strokeWidth="2" stroke="currentColor" width="13" height="13">
        <polyline points="4 17 10 11 4 5"/>
        <line x1="12" y1="19" x2="20" y2="19"/>
    </svg>
);

const ChevronIcon = ({open}: { open: boolean }) => (
    <svg
        viewBox="0 0 24 24" fill="none" strokeWidth="2" stroke="currentColor"
        width="12" height="12"
        style={{transform: open ? "rotate(180deg)" : "none", transition: "transform .2s"}}
    >
        <polyline points="6 9 12 15 18 9"/>
    </svg>
);

function classifyLine(line: string): "error" | "warning" | "success" | "info" {
    const l = line.toLowerCase();
    if (l.includes("error") || l.includes("failed")) return "error";
    if (l.includes("warning") || l.includes("warn")) return "warning";
    if (l.includes("100%") || l.includes("destination") || l.includes("finished")) return "success";
    return "info";
}

function extractYouTubeId(url: string): string | null {
    try {
        const u = new URL(url);
        if (u.hostname.includes("youtu.be")) return u.pathname.slice(1).split("/")[0] || null;
        if (u.hostname.includes("youtube.com")) {
            // Regular videos — ?v=ID; shorts/embed/live carry the ID in the path.
            const v = u.searchParams.get("v");
            if (v) return v;
            const m = u.pathname.match(/^\/(?:shorts|embed|live)\/([^/?#]+)/);
            return m ? m[1] : null;
        }
        return null;
    } catch {
        return null;
    }
}

// The embedded YouTube player (iframe) doesn't work in Tauri on Linux/webkit:
// YouTube rejects the embed because tauri://localhost has no valid Referer
// (open bug tauri#14422). So show a thumbnail preview instead, and open the
// actual player in an external browser — a plain <img> isn't subject to this restriction.
function YouTubePreview({videoId, url, onDownloadPreview, previewDownloading}: {
    videoId: string;
    url: string;
    onDownloadPreview: () => void;
    previewDownloading: boolean;
}) {
    const thumb = `https://i.ytimg.com/vi/${videoId}/hqdefault.jpg`;
    return (
        <div className="thumb-wrap">
            <img
                src={thumb}
                alt="Video preview"
                style={{cursor: "pointer"}}
                onClick={async () => await openUrl(url)}
            />
            <button className="btn-primary btn-primary-link" onClick={onDownloadPreview} disabled={previewDownloading}>
                {previewDownloading ? "Saving preview…" : "⬇ Save preview"}
            </button>
        </div>
    );
}

const LogPanel = memo(function LogPanel({logs, downloading}: {
    logs: { text: string; kind: "error" | "warning" | "success" | "info" }[];
    downloading: boolean;
}) {
    const [open, setOpen] = useState(false);
    const logsEndRef = useRef<HTMLDivElement>(null);
    const errorCount = useMemo(() => logs.filter((l) => l.kind === "error").length, [logs]);

    useEffect(() => {
        if (open) logsEndRef.current?.scrollIntoView({behavior: "smooth"});
    }, [logs, open]);

    if (!logs.length) return null;

    return (
        <div className="log-panel">
            <button className="log-toggle" onClick={() => setOpen((v) => !v)}>
                <span className="log-toggle-left">
                    <TerminalIcon/>
                    <span>yt-dlp logs</span>
                    {errorCount > 0 && <span className="log-badge-error">{errorCount} err.</span>}
                    {downloading && <span className="log-live-dot"/>}
                </span>
                <span className="log-toggle-right">
                    <span className="log-count">{logs.length} lines</span>
                    <ChevronIcon open={open}/>
                </span>
            </button>
            {open && (
                <div className="log-body">
                    {logs.map((l, i) => (
                        <div key={i} className={`log-line log-line--${l.kind}`}>
                            <span className="log-line-num">{i + 1}</span>
                            <span className="log-line-text">{l.text}</span>
                        </div>
                    ))}
                    <div ref={logsEndRef}/>
                </div>
            )}
        </div>
    );
});

export default function App() {
    const [url, setUrl] = useState("");
    const [urlKind, setUrlKind] = useState<UrlKind>(null);
    const [platform, setPlatform] = useState<Platform>(null);

    // single video
    const [youtubeInfo, setYoutubeInfo] = useState<YoutubeInfo | null>(null);
    const [twitchInfo, setTwitchInfo] = useState<TwitchInfo | null>(null);
    const [startTime, setStartTime] = useState("00:00:00");
    const [endTime, setEndTime] = useState("00:00:00");
    const [timeError, setTimeError] = useState("");
    const [result, setResult] = useState<DownloadResult | null>(null);

    // playlist
    const [playlistInfo, setPlaylistInfo] = useState<PlaylistInfo | null>(null);
    const [tracksExpanded, setTracksExpanded] = useState(false);
    const [playlistResult, setPlaylistResult] = useState<PlaylistDownloadResult | null>(null);

    // shared
    const [quality, setQuality] = useState<Quality>("best");
    const [mode, setMode] = useState<DownloadMode>("video");
    const [audioTracks, setAudioTracks] = useState<string[]>([]);   // audio track language codes
    const [audioLang, setAudioLang] = useState<string | null>(null);
    const [videoCodecs, setVideoCodecs] = useState<string[]>([]);   // available video codecs
    const [videoCodec, setVideoCodec] = useState<string | null>(null); // null = auto
    const [audioCodecs, setAudioCodecs] = useState<string[]>([]);   // available audio codecs
    const [audioCodec, setAudioCodec] = useState<string | null>(null);
    const [urlError, setUrlError] = useState("");
    const [downloading, setDownloading] = useState(false);
    const [error, setError] = useState("");
    const [loadingInfo, setLoadingInfo] = useState(false);
    const [downloadPath, setDownloadPath] = useState<string | null>(null);
    const [logs, setLogs] = useState<{ text: string; kind: "error" | "warning" | "success" | "info" }[]>([]);
    const [previewDownloading, setPreviewDownloading] = useState(false);
    const [previewResult, setPreviewResult] = useState<DownloadResult | null>(null);

    // Binary readiness (yt-dlp/ffmpeg). Show a loading screen until it's ready.
    const [depsReady, setDepsReady] = useState(false);
    const [depsError, setDepsError] = useState<string | null>(null);

    const abortRef = useRef<AbortController | null>(null);
    const unlistenRef = useRef<(() => void) | null>(null);

    useEffect(() => {
        let cancelled = false;
        // During a download, "ytdlp-log" events arrive in rapid real-time bursts;
        // re-rendering the log panel on every single event is wasteful — buffer
        // them and flush once per animation frame. On WebView2 (Windows) this
        // is especially noticeable: without batching, the log panel stutters
        // and visibly lags under a fast event stream.
        let buffer: { text: string; kind: "error" | "warning" | "success" | "info" }[] = [];
        let rafId = 0;

        const flush = () => {
            rafId = 0;
            if (!buffer.length) return;
            const incoming = buffer;
            buffer = [];
            setLogs((prev) => [...prev, ...incoming].slice(-500));
        };

        listen<string>("ytdlp-log", (event) => {
            if (cancelled) return;
            buffer.push({text: event.payload, kind: classifyLine(event.payload)});
            if (!rafId) rafId = requestAnimationFrame(flush);
        }).then((unlisten) => {
            if (cancelled) unlisten();
            else unlistenRef.current = unlisten;
        });
        return () => {
            cancelled = true;
            if (rafId) cancelAnimationFrame(rafId);
            unlistenRef.current?.();
        };
    }, []);

    // Binary readiness: subscribe to events first, then ask for the current
    // state (install may have finished before we subscribed).
    useEffect(() => {
        let cancelled = false;
        const unlisteners: (() => void)[] = [];
        listen("deps-ready", () => !cancelled && setDepsReady(true)).then((u) => unlisteners.push(u));
        listen<string>("deps-error", (e) => !cancelled && setDepsError(e.payload)).then((u) => unlisteners.push(u));
        invoke<boolean>("deps_ready").then((ready) => {
            if (!cancelled && ready) setDepsReady(true);
        });
        return () => {
            cancelled = true;
            unlisteners.forEach((u) => u());
        };
    }, []);

    // Auto-detect URL type and fetch info
    useEffect(() => {
        abortRef.current?.abort();

        if (!url.trim()) {
            resetAll();
            return;
        }

        setYoutubeInfo(null);
        setTwitchInfo(null);
        setPlaylistInfo(null);
        setPlatform(null);
        setUrlKind(null);
        setUrlError("");
        setResult(null);
        setPlaylistResult(null);
        setPreviewResult(null);
        setError("");
        // Reset the clip range, otherwise it "sticks" from the previous video
        // when the new one's duration can't be resolved (common on Windows).
        setStartTime("00:00:00");
        setEndTime("00:00:00");
        setTimeError("");
        setAudioTracks([]);
        setAudioLang(null);
        setVideoCodecs([]);
        setAudioCodecs([]);
        setVideoCodec(null);
        setAudioCodec(null);

        const controller = new AbortController();
        abortRef.current = controller;

        const timer = setTimeout(async () => {
            setLoadingInfo(true);
            try {
                const [isYt, isTw, isPl] = await Promise.all([
                    invoke<boolean>("is_youtube_url", {url}),
                    invoke<boolean>("is_twitch_url", {url}),
                    invoke<boolean>("is_playlist_url", {url}),
                ]);
                if (controller.signal.aborted) return;

                if (isPl) {
                    setUrlKind("playlist");
                    setPlatform("youtube");
                    const info = await invoke<PlaylistInfo>("get_playlist_info", {url});
                    if (!controller.signal.aborted) setPlaylistInfo(info);
                } else if (isYt) {
                    setUrlKind("single");
                    setPlatform("youtube");
                    const info = await invoke<YoutubeInfo>("get_youtube_info", {url});
                    if (!controller.signal.aborted) {
                        setYoutubeInfo(info);
                        setAudioTracks(info.audio_tracks ?? []);
                        setVideoCodecs(info.video_codecs ?? []);
                        setAudioCodecs(info.audio_codecs ?? []);
                        const d = typeof info.duration === "number" ? info.duration : null;
                        if (d != null && d > 0) {
                            setStartTime("00:00:00");
                            setEndTime(formatDuration(d));
                        }
                    }
                } else if (isTw) {
                    setUrlKind("single");
                    setPlatform("twitch");
                    const info = await invoke<TwitchInfo>("get_twitch_info", {url});
                    if (!controller.signal.aborted) {
                        setTwitchInfo(info);
                        setAudioTracks(info.audio_tracks ?? []);
                        setVideoCodecs(info.video_codecs ?? []);
                        setAudioCodecs(info.audio_codecs ?? []);
                        const d = typeof info.duration === "number" ? info.duration : null;
                        if (d != null && d > 0) {
                            setStartTime("00:00:00");
                            setEndTime(formatDuration(d));
                        }
                    }
                } else {
                    if (!controller.signal.aborted)
                        setUrlError("Invalid URL — YouTube and Twitch are supported");
                }
            } catch (e) {
                if (!controller.signal.aborted) {
                    const reason = typeof e === "string" && e.trim() ? e : "Failed to fetch video info";
                    setUrlError(reason);
                }
            } finally {
                if (!controller.signal.aborted) setLoadingInfo(false);
            }
        }, 600);

        return () => {
            clearTimeout(timer);
            controller.abort();
            setLoadingInfo(false);
        };
    }, [url]);

    function resetAll() {
        setYoutubeInfo(null);
        setTwitchInfo(null);
        setPlaylistInfo(null);
        setPlatform(null);
        setUrlKind(null);
        setUrlError("");
        setResult(null);
        setPlaylistResult(null);
        setPreviewResult(null);
        setError("");
        setLoadingInfo(false);
        setDownloadPath(null);
        setStartTime("00:00:00");
        setEndTime("00:00:00");
        setTimeError("");
        setAudioTracks([]);
        setAudioLang(null);
        setVideoCodecs([]);
        setAudioCodecs([]);
        setVideoCodec(null);
        setAudioCodec(null);
    }

    const handleDownload = useCallback(async () => {
        if (!platform || downloading || urlKind !== "single") return;
        setTimeError("");
        setError("");
        setLogs([]);

        try {
            await invoke("validate_time_range", {
                start: startTime,
                end: endTime,
                maxDuration: youtubeInfo?.duration ?? twitchInfo?.duration ?? null,
            });
        } catch (e) {
            setTimeError(String(e));
            return;
        }

        setDownloading(true);
        setResult(null);

        try {
            const cmd = platform === "twitch" ? "download_twitch" : "download_video";
            const res = await invoke<DownloadResult>(cmd, {
                url, quality, mode,
                path: downloadPath ?? null,
                start: startTime, end: endTime,
                duration: youtubeInfo?.duration ?? twitchInfo?.duration ?? null,
                audioLang: audioLang ?? null,
                videoCodec: videoCodec ?? null,
                audioCodec: audioCodec ?? null,
            });
            setResult(res);
        } catch (e) {
            const msg = typeof e === "string" ? e : String(e);
            if (msg !== CANCEL_MSG) setError(msg);   // don't show a cancel as an error
        } finally {
            setDownloading(false);
        }
    }, [url, platform, urlKind, quality, mode, downloading, downloadPath, startTime, endTime, youtubeInfo, twitchInfo, audioLang, videoCodec, audioCodec]);

    const handlePlaylistDownload = useCallback(async () => {
        if (!playlistInfo || downloading) return;
        setError("");
        setLogs([]);
        setDownloading(true);
        setPlaylistResult(null);

        try {
            const res = await invoke<PlaylistDownloadResult>("download_playlist", {
                url, quality, mode,
                path: downloadPath ?? null,
            });
            setPlaylistResult(res);
        } catch (e) {
            const msg = typeof e === "string" ? e : String(e);
            if (msg !== CANCEL_MSG) setError(msg);   // don't show a cancel as an error
        } finally {
            setDownloading(false);
        }
    }, [url, playlistInfo, quality, mode, downloadPath, downloading]);

    const handleDownloadPreview = useCallback(async (thumbUrl: string, fallbackUrl?: string) => {
        if (previewDownloading) return;
        setError("");
        setPreviewResult(null);
        setPreviewDownloading(true);
        try {
            const res = await invoke<DownloadResult>("download_preview", {
                url: thumbUrl,
                fallbackUrl: fallbackUrl ?? null,
                title: youtubeInfo?.title ?? twitchInfo?.title ?? "",
                path: downloadPath ?? null,
            });
            setPreviewResult(res);
        } catch (e) {
            const msg = typeof e === "string" ? e : String(e);
            setError(msg);
        } finally {
            setPreviewDownloading(false);
        }
    }, [previewDownloading, youtubeInfo, twitchInfo, downloadPath]);

    const handleCancel = useCallback(async () => {
        try {
            await invoke("cancel_download");
        } catch {
            /* ignore — the command only sends a cancellation signal */
        }
    }, []);

    const hasInfo = youtubeInfo !== null || twitchInfo !== null;
    const videoTitle = youtubeInfo?.title ?? twitchInfo?.title ?? "";
    const videoSub = youtubeInfo
        ? [`By ${youtubeInfo.author_name}`, youtubeInfo.duration != null ? formatDuration(Number(youtubeInfo.duration)) : ""].filter(Boolean).join("  ·  ")
        : twitchInfo
            ? [twitchInfo.channel, twitchInfo.is_live ? "🔴 Live" : "", twitchInfo.duration ? formatDuration(twitchInfo.duration) : ""].filter(Boolean).join("  ·  ")
            : "";

    const singleDownloadLabel = () => {
        if (downloading) return <><span className="spinner"/>Downloading…</>;
        if (twitchInfo?.is_live) return mode === "audio" ? "Record audio" : "Record stream";
        return mode === "audio" ? "Download audio" : "Download video";
    };

    const playlistDownloadLabel = () => {
        if (downloading) return <><span className="spinner"/>Downloading…</>;
        return mode === "audio" ? "Download playlist audio" : "Download playlist";
    };

    if (!depsReady) {
        return (
            <main className="app deps-screen">
                <div className="logo">
                    <div className="logo-icon">
                        <svg viewBox="0 0 24 24">
                            <path
                                d="M12 2C6.48 2 2 6.48 2 12s4.48 10 10 10 10-4.48 10-10S17.52 2 12 2zm-2 14.5v-9l6 4.5-6 4.5z"/>
                        </svg>
                    </div>
                    <span className="logo-name">Flowbit</span>
                    <span className="logo-tag">beta</span>
                </div>
                <div className="deps-loader">
                    {depsError ? (
                        <>
                            <div className="deps-error-icon">
                                <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2"
                                     width="28" height="28">
                                    <circle cx="12" cy="12" r="10"/>
                                    <line x1="12" y1="8" x2="12" y2="12"/>
                                    <line x1="12" y1="16" x2="12.01" y2="16"/>
                                </svg>
                            </div>
                            <p className="deps-title">Failed to load components</p>
                            <p className="deps-sub">{depsError}</p>
                            <p className="deps-sub">Check your internet connection and restart the app</p>
                        </>
                    ) : (
                        <>
                            <div className="deps-spinner"/>
                            <p className="deps-title">Getting ready…</p>
                            <p className="deps-sub">Downloading yt-dlp and ffmpeg — only needed on first
                                launch</p>
                        </>
                    )}
                </div>
            </main>
        );
    }

    return (
        <main className="app">
            <div className="logo">
                <div className="logo-icon">
                    <svg viewBox="0 0 24 24">
                        <path
                            d="M12 2C6.48 2 2 6.48 2 12s4.48 10 10 10 10-4.48 10-10S17.52 2 12 2zm-2 14.5v-9l6 4.5-6 4.5z"/>
                    </svg>
                </div>
                <span className="logo-name">Flowbit</span>
                <span className="logo-tag">beta</span>
            </div>

            <div className="search-box">
                <div className="search-icon"><SearchIcon/></div>
                <input
                    className={`url-input${urlError ? " url-input--error" : ""}`}
                    placeholder="Paste a YouTube or Twitch URL…"
                    value={url}
                    onChange={(e) => setUrl(e.target.value)}
                    disabled={downloading}
                />
                {loadingInfo && <div className="search-spinner"/>}
            </div>

            {urlError && (
                <p className="url-error">
                    <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" width="13" height="13">
                        <circle cx="12" cy="12" r="10"/>
                        <line x1="12" y1="8" x2="12" y2="12"/>
                        <line x1="12" y1="16" x2="12.01" y2="16"/>
                    </svg>
                    {urlError}
                </p>
            )}

            {!hasInfo && !playlistInfo && !urlError && !loadingInfo && (
                <div className="empty-state">
                    <VideoIcon/>
                    <p>YouTube and Twitch are supported<br/>Paste a link above to get started</p>
                </div>
            )}

            {loadingInfo && (
                <div className="card skeleton-card">
                    <div className="skeleton-header">
                        <div className="skeleton skeleton-badge"/>
                        <div className="skeleton skeleton-title"/>
                        <div className="skeleton skeleton-sub"/>
                    </div>
                    <div className="skeleton skeleton-thumb"/>
                </div>
            )}

            {/* ── Single video card ── */}
            {hasInfo && urlKind === "single" && (
                <div className="card">
                    <div className="card-header">
                        <span className={`badge badge-${platform}`}>
                            {platform === "twitch" ? "Twitch" : "YouTube"}
                        </span>
                        <h2 className="card-title">{videoTitle}</h2>
                        <p className="card-sub">{videoSub}</p>
                    </div>

                    {youtubeInfo && (() => {
                        const videoId = extractYouTubeId(url);
                        if (!videoId) return null;
                        return (
                            <YouTubePreview
                                videoId={videoId}
                                url={url}
                                previewDownloading={previewDownloading}
                                onDownloadPreview={() => handleDownloadPreview(
                                    `https://i.ytimg.com/vi/${videoId}/maxresdefault.jpg`,
                                    `https://i.ytimg.com/vi/${videoId}/hqdefault.jpg`,
                                )}
                            />
                        );
                    })()}

                    {twitchInfo?.thumbnail_url && (
                        <div className="thumb-wrap">
                            <img src={twitchInfo.thumbnail_url} alt={twitchInfo.title}/>
                            <button className="btn-primary btn-primary-link" onClick={async () => await openUrl(url)}>
                                Open on Twitch
                            </button>
                            <button
                                className="btn-primary btn-primary-link"
                                onClick={() => handleDownloadPreview(twitchInfo.thumbnail_url!)}
                                disabled={previewDownloading}
                            >
                                {previewDownloading ? "Saving preview…" : "⬇ Save preview"}
                            </button>
                        </div>
                    )}

                    <div className="divider"/>

                    <div className="quality-section">
                        <p className="quality-label">Format</p>
                        <div className="mode-toggle">
                            <button className={`mode-btn${mode === "video" ? " active" : ""}`}
                                    onClick={() => setMode("video")} disabled={downloading}>
                                <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" width="14"
                                     height="14">
                                    <path d="M15 10l4.553-2.07A1 1 0 0121 8.81v6.38a1 1 0 01-1.447.9L15 14"/>
                                    <rect x="3" y="6" width="12" height="12" rx="2"/>
                                </svg>
                                Video
                            </button>
                            <button className={`mode-btn${mode === "audio" ? " active" : ""}`}
                                    onClick={() => setMode("audio")} disabled={downloading}>
                                <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" width="14"
                                     height="14">
                                    <path d="M9 18V5l12-2v13"/>
                                    <circle cx="6" cy="18" r="3"/>
                                    <circle cx="18" cy="16" r="3"/>
                                </svg>
                                Audio only
                            </button>
                        </div>
                    </div>

                    {audioTracks.length > 1 && (
                        <div className="quality-section" style={{paddingTop: 0}}>
                            <p className="quality-label">Audio track</p>
                            <div className="quality-pills">
                                <button
                                    className={`quality-pill${audioLang === null ? " active" : ""}`}
                                    onClick={() => setAudioLang(null)} disabled={downloading}>
                                    Auto
                                </button>
                                {audioTracks.map((code) => (
                                    <button key={code}
                                            className={`quality-pill${audioLang === code ? " active" : ""}`}
                                            onClick={() => setAudioLang(code)} disabled={downloading}>
                                        {langName(code)}
                                    </button>
                                ))}
                            </div>
                        </div>
                    )}

                    {mode === "video" && videoCodecs.length > 1 && (
                        <div className="quality-section" style={{paddingTop: 0}}>
                            <p className="quality-label">Video codec</p>
                            <div className="quality-pills">
                                <button className={`quality-pill${videoCodec === null ? " active" : ""}`}
                                        onClick={() => setVideoCodec(null)} disabled={downloading}>
                                    Auto
                                </button>
                                {videoCodecs.map((c) => (
                                    <button key={c}
                                            className={`quality-pill${videoCodec === c ? " active" : ""}`}
                                            onClick={() => setVideoCodec(c)} disabled={downloading}>
                                        {VCODEC_NAMES[c] ?? c}
                                    </button>
                                ))}
                            </div>
                        </div>
                    )}

                    {audioCodecs.length > 1 && (
                        <div className="quality-section" style={{paddingTop: 0}}>
                            <p className="quality-label">Audio codec</p>
                            <div className="quality-pills">
                                <button className={`quality-pill${audioCodec === null ? " active" : ""}`}
                                        onClick={() => setAudioCodec(null)} disabled={downloading}>
                                    Auto
                                </button>
                                {audioCodecs.map((c) => (
                                    <button key={c}
                                            className={`quality-pill${audioCodec === c ? " active" : ""}`}
                                            onClick={() => setAudioCodec(c)} disabled={downloading}>
                                        {ACODEC_NAMES[c] ?? c}
                                    </button>
                                ))}
                            </div>
                        </div>
                    )}

                    {mode === "video" && (
                        <div className="quality-section" style={{paddingTop: 0}}>
                            <p className="quality-label">Quality</p>
                            <div className="quality-pills">
                                {QUALITIES.map((q) => (
                                    <button key={q.value}
                                            className={`quality-pill${quality === q.value ? " active" : ""}`}
                                            onClick={() => setQuality(q.value)} disabled={downloading}>
                                        {q.label}
                                    </button>
                                ))}
                            </div>
                        </div>
                    )}

                    <div className="quality-section" style={{paddingTop: 0}}>
                        <p className="quality-label">Clip range</p>
                        <div style={{display: "flex", gap: "8px"}}>
                            <input className="url-input" placeholder="00:00:00" value={startTime}
                                   onChange={(e) => setStartTime(e.target.value)}/>
                            <input className="url-input" value={endTime}
                                   onChange={(e) => setEndTime(e.target.value)}/>
                        </div>
                        {timeError && <p className="url-error">{timeError}</p>}

                        <p className="patch-label">Save folder</p>
                        <button className="patch-pill" onClick={async () => {
                            const selected = await open({
                                directory: true,
                                multiple: false,
                                defaultPath: downloadPath ?? undefined
                            });
                            if (typeof selected === "string") setDownloadPath(selected);
                        }}>
                            📁 {downloadPath ? downloadPath.split(/[\\/]/).filter(Boolean).pop() : "Downloads (default)"}
                        </button>
                    </div>

                    {error && (
                        <div className="error-box">
                            <div className="error-title">
                                <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" width="14"
                                     height="14">
                                    <circle cx="12" cy="12" r="10"/>
                                    <line x1="12" y1="8" x2="12" y2="12"/>
                                    <line x1="12" y1="16" x2="12.01" y2="16"/>
                                </svg>
                                Download failed
                            </div>
                            <p className="error-msg">{error}</p>
                        </div>
                    )}

                    {result && (
                        <div className="result-box">
                            <div className="result-title">
                                <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" width="14"
                                     height="14">
                                    <polyline points="20 6 9 17 4 12"/>
                                </svg>
                                File saved
                            </div>
                            <p className="result-path">{result.path}</p>
                            <p className="result-size">{result.file_size_mb.toFixed(1)} MB</p>
                        </div>
                    )}

                    {previewResult && (
                        <div className="result-box">
                            <div className="result-title">
                                <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" width="14"
                                     height="14">
                                    <polyline points="20 6 9 17 4 12"/>
                                </svg>
                                Preview saved
                            </div>
                            <p className="result-path">{previewResult.path}</p>
                            <p className="result-size">{previewResult.file_size_mb.toFixed(1)} MB</p>
                        </div>
                    )}

                    <LogPanel logs={logs} downloading={downloading}/>

                    <div className="card-footer">
                        <button className="btn-primary" onClick={handleDownload} disabled={downloading}>
                            {singleDownloadLabel()}
                        </button>
                        {downloading && (
                            <button className="btn-cancel" onClick={handleCancel}>Cancel</button>
                        )}
                    </div>
                </div>
            )}

            {/* ── Playlist card ── */}
            {playlistInfo && urlKind === "playlist" && (
                <div className="card">
                    <div className="card-header">
                        <span className="badge badge-playlist">Playlist</span>
                        <h2 className="card-title">{playlistInfo.title}</h2>
                        <p className="card-sub">
                            {playlistInfo.uploader && `${playlistInfo.uploader}  ·  `}
                            {playlistInfo.count} videos
                        </p>
                    </div>

                    <div className="playlist-tracks">
                        <button className="log-toggle playlist-toggle" onClick={() => setTracksExpanded(v => !v)}>
                            <span className="log-toggle-left">
                                <svg viewBox="0 0 24 24" fill="none" strokeWidth="2" stroke="currentColor" width="13"
                                     height="13">
                                    <line x1="8" y1="6" x2="21" y2="6"/>
                                    <line x1="8" y1="12" x2="21" y2="12"/>
                                    <line x1="8" y1="18" x2="21" y2="18"/>
                                    <line x1="3" y1="6" x2="3.01" y2="6"/>
                                    <line x1="3" y1="12" x2="3.01" y2="12"/>
                                    <line x1="3" y1="18" x2="3.01" y2="18"/>
                                </svg>
                                <span>Playlist tracks</span>
                            </span>
                            <span className="log-toggle-right">
                                <span className="log-count">{playlistInfo.entries.length} tracks</span>
                                <ChevronIcon open={tracksExpanded}/>
                            </span>
                        </button>
                        {tracksExpanded && (
                            <div className="playlist-entries">
                                {playlistInfo.entries.map((entry, i) => (
                                    <div key={entry.id} className="playlist-entry">
                                        <span className="playlist-entry-num">{i + 1}</span>
                                        <span className="playlist-entry-title">{entry.title}</span>
                                        {entry.duration != null && (
                                            <span className="playlist-entry-dur">{formatDuration(entry.duration)}</span>
                                        )}
                                    </div>
                                ))}
                            </div>
                        )}
                    </div>

                    <div className="divider"/>

                    <div className="quality-section">
                        <p className="quality-label">Format</p>
                        <div className="mode-toggle">
                            <button className={`mode-btn${mode === "video" ? " active" : ""}`}
                                    onClick={() => setMode("video")} disabled={downloading}>
                                <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" width="14"
                                     height="14">
                                    <path d="M15 10l4.553-2.07A1 1 0 0121 8.81v6.38a1 1 0 01-1.447.9L15 14"/>
                                    <rect x="3" y="6" width="12" height="12" rx="2"/>
                                </svg>
                                Video
                            </button>
                            <button className={`mode-btn${mode === "audio" ? " active" : ""}`}
                                    onClick={() => setMode("audio")} disabled={downloading}>
                                <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" width="14"
                                     height="14">
                                    <path d="M9 18V5l12-2v13"/>
                                    <circle cx="6" cy="18" r="3"/>
                                    <circle cx="18" cy="16" r="3"/>
                                </svg>
                                Audio only
                            </button>
                        </div>
                    </div>

                    {audioTracks.length > 1 && (
                        <div className="quality-section" style={{paddingTop: 0}}>
                            <p className="quality-label">Audio track</p>
                            <div className="quality-pills">
                                <button
                                    className={`quality-pill${audioLang === null ? " active" : ""}`}
                                    onClick={() => setAudioLang(null)} disabled={downloading}>
                                    Auto
                                </button>
                                {audioTracks.map((code) => (
                                    <button key={code}
                                            className={`quality-pill${audioLang === code ? " active" : ""}`}
                                            onClick={() => setAudioLang(code)} disabled={downloading}>
                                        {langName(code)}
                                    </button>
                                ))}
                            </div>
                        </div>
                    )}

                    {mode === "video" && (
                        <div className="quality-section" style={{paddingTop: 0}}>
                            <p className="quality-label">Quality</p>
                            <div className="quality-pills">
                                {QUALITIES.map((q) => (
                                    <button key={q.value}
                                            className={`quality-pill${quality === q.value ? " active" : ""}`}
                                            onClick={() => setQuality(q.value)} disabled={downloading}>
                                        {q.label}
                                    </button>
                                ))}
                            </div>
                        </div>
                    )}

                    <div className="quality-section" style={{paddingTop: 0}}>
                        <p className="patch-label">Save folder</p>
                        <button className="patch-pill" onClick={async () => {
                            const selected = await open({
                                directory: true,
                                multiple: false,
                                defaultPath: downloadPath ?? undefined
                            });
                            if (typeof selected === "string") setDownloadPath(selected);
                        }}>
                            📁 {downloadPath ? downloadPath.split(/[\\/]/).filter(Boolean).pop() : "Downloads (default)"}
                        </button>
                        <p className="path-hint">The playlist will be saved in its own subfolder inside the
                            selected directory</p>
                    </div>

                    {error && (
                        <div className="error-box">
                            <div className="error-title">
                                <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" width="14"
                                     height="14">
                                    <circle cx="12" cy="12" r="10"/>
                                    <line x1="12" y1="8" x2="12" y2="12"/>
                                    <line x1="12" y1="16" x2="12.01" y2="16"/>
                                </svg>
                                Download failed
                            </div>
                            <p className="error-msg">{error}</p>
                        </div>
                    )}

                    {playlistResult && (
                        <div className="result-box">
                            <div className="result-title">
                                <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" width="14"
                                     height="14">
                                    <polyline points="20 6 9 17 4 12"/>
                                </svg>
                                Playlist saved
                            </div>
                            <p className="result-path">{playlistResult.dir}</p>
                            <p className="result-size">
                                {playlistResult.downloaded} files · {playlistResult.total_size_mb.toFixed(1)} MB
                            </p>
                        </div>
                    )}

                    <LogPanel logs={logs} downloading={downloading}/>

                    <div className="card-footer">
                        <button className="btn-primary" onClick={handlePlaylistDownload} disabled={downloading}>
                            {playlistDownloadLabel()}
                        </button>
                        {downloading && (
                            <button className="btn-cancel" onClick={handleCancel}>Cancel</button>
                        )}
                    </div>
                </div>
            )}
        </main>
    );
}
