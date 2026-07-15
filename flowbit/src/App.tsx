import "./App.css";
import {useState, useEffect, useCallback, useRef} from "react";
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

// Должно совпадать с CANCEL_MSG в бэкенде (youtube.rs).
const CANCEL_MSG = "Загрузка отменена";

// Код языка ("en", "ru", "en-US") → название на русском ("английский", …).
function langName(code: string): string {
    try {
        return new Intl.DisplayNames(["ru"], {type: "language"}).of(code) ?? code;
    } catch {
        return code;
    }
}

const QUALITIES: { value: Quality; label: string }[] = [
    {value: "best", label: "Лучшее"},
    {value: "high", label: "1080p"},
    {value: "medium", label: "720p"},
    {value: "low", label: "480p"},
    {value: "worst", label: "Худшее"},
];

// Понятные названия кодеков (короткие имена приходят с бэкенда).
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
    if (l.includes("error") || l.includes("failed") || l.includes("ошибка")) return "error";
    if (l.includes("warning") || l.includes("warn")) return "warning";
    if (l.includes("100%") || l.includes("destination") || l.includes("finished")) return "success";
    return "info";
}

function extractYouTubeId(url: string): string | null {
    try {
        const u = new URL(url);
        if (u.hostname.includes("youtu.be")) return u.pathname.slice(1).split("/")[0] || null;
        if (u.hostname.includes("youtube.com")) {
            // Обычные ролики — ?v=ID; shorts/embed/live хранят ID в пути.
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

// Встроенный YouTube-плеер (iframe) не работает в Tauri на Linux/webkit: YouTube
// отклоняет embed из-за отсутствия валидного Referer у протокола tauri://localhost
// (открытый баг tauri#14422). Поэтому показываем превью-обложку, а сам плеер
// открываем во внешнем браузере — обычный <img> под это ограничение не попадает.
function YouTubePreview({videoId, url}: { videoId: string; url: string }) {
    const thumb = `https://i.ytimg.com/vi/${videoId}/hqdefault.jpg`;
    return (
        <div className="thumb-wrap">
            <img
                src={thumb}
                alt="Превью видео"
                style={{cursor: "pointer"}}
                onClick={async () => await openUrl(url)}
            />
        </div>
    );
}

function LogPanel({logs, downloading}: {
    logs: { text: string; kind: "error" | "warning" | "success" | "info" }[];
    downloading: boolean;
}) {
    const [open, setOpen] = useState(false);
    const logsEndRef = useRef<HTMLDivElement>(null);
    const errorCount = logs.filter((l) => l.kind === "error").length;

    useEffect(() => {
        if (open) logsEndRef.current?.scrollIntoView({behavior: "smooth"});
    }, [logs, open]);

    if (!logs.length) return null;

    return (
        <div className="log-panel">
            <button className="log-toggle" onClick={() => setOpen((v) => !v)}>
                <span className="log-toggle-left">
                    <TerminalIcon/>
                    <span>Логи yt-dlp</span>
                    {errorCount > 0 && <span className="log-badge-error">{errorCount} ошиб.</span>}
                    {downloading && <span className="log-live-dot"/>}
                </span>
                <span className="log-toggle-right">
                    <span className="log-count">{logs.length} строк</span>
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
}

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
    const [audioTracks, setAudioTracks] = useState<string[]>([]);   // коды языков аудиодорожек
    const [audioLang, setAudioLang] = useState<string | null>(null);
    const [videoCodecs, setVideoCodecs] = useState<string[]>([]);   // доступные кодеки видео
    const [audioCodecs, setAudioCodecs] = useState<string[]>([]);   // доступные кодеки аудио
    const [videoCodec, setVideoCodec] = useState<string | null>(null); // null = авто
    const [audioCodec, setAudioCodec] = useState<string | null>(null);
    const [urlError, setUrlError] = useState("");
    const [downloading, setDownloading] = useState(false);
    const [error, setError] = useState("");
    const [loadingInfo, setLoadingInfo] = useState(false);
    const [downloadPath, setDownloadPath] = useState<string | null>(null);
    const [logs, setLogs] = useState<{ text: string; kind: "error" | "warning" | "success" | "info" }[]>([]);

    const abortRef = useRef<AbortController | null>(null);
    const unlistenRef = useRef<(() => void) | null>(null);

    useEffect(() => {
        let cancelled = false;
        listen<string>("ytdlp-log", (event) => {
            if (cancelled) return;
            setLogs((prev) => [...prev.slice(-499), {text: event.payload, kind: classifyLine(event.payload)}]);
        }).then((unlisten) => {
            if (cancelled) unlisten();
            else unlistenRef.current = unlisten;
        });
        return () => {
            cancelled = true;
            unlistenRef.current?.();
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
        setError("");
        // Сбрасываем время фрагмента, иначе оно "залипает" от прошлого видео,
        // если у нового длительность не получена (частый случай на Windows).
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
                        setUrlError("Неверный URL — поддерживаются YouTube и Twitch");
                }
            } catch {
                if (!controller.signal.aborted)
                    setUrlError("Не удалось получить информацию о видео");
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
            if (msg !== CANCEL_MSG) setError(msg);   // отмену не показываем как ошибку
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
            if (msg !== CANCEL_MSG) setError(msg);   // отмену не показываем как ошибку
        } finally {
            setDownloading(false);
        }
    }, [url, playlistInfo, quality, mode, downloadPath, downloading]);

    const handleCancel = useCallback(async () => {
        try {
            await invoke("cancel_download");
        } catch {
            /* игнорируем — команда только шлёт сигнал отмены */
        }
    }, []);

    const hasInfo = youtubeInfo !== null || twitchInfo !== null;
    const videoTitle = youtubeInfo?.title ?? twitchInfo?.title ?? "";
    const videoSub = youtubeInfo
        ? [`Автор: ${youtubeInfo.author_name}`, youtubeInfo.duration != null ? formatDuration(Number(youtubeInfo.duration)) : ""].filter(Boolean).join("  ·  ")
        : twitchInfo
            ? [twitchInfo.channel, twitchInfo.is_live ? "🔴 Live" : "", twitchInfo.duration ? formatDuration(twitchInfo.duration) : ""].filter(Boolean).join("  ·  ")
            : "";

    const singleDownloadLabel = () => {
        if (downloading) return <><span className="spinner"/>Загружается…</>;
        if (twitchInfo?.is_live) return mode === "audio" ? "Записать аудио" : "Записать стрим";
        return mode === "audio" ? "Скачать аудио" : "Скачать видео";
    };

    const playlistDownloadLabel = () => {
        if (downloading) return <><span className="spinner"/>Загружается…</>;
        return mode === "audio" ? "Скачать аудио плейлиста" : "Скачать плейлист";
    };

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
                    placeholder="Вставьте YouTube или Twitch URL…"
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
                    <p>Поддерживается YouTube и Twitch<br/>Вставьте ссылку выше для начала</p>
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
                        return <YouTubePreview videoId={videoId} url={url}/>;
                    })()}

                    {twitchInfo?.thumbnail_url && (
                        <div className="thumb-wrap">
                            <img src={twitchInfo.thumbnail_url} alt={twitchInfo.title}/>
                            <button className="btn-primary btn-primary-link" onClick={async () => await openUrl(url)}>
                                Открыть в Twitch
                            </button>
                        </div>
                    )}

                    <div className="divider"/>

                    <div className="quality-section">
                        <p className="quality-label">Формат</p>
                        <div className="mode-toggle">
                            <button className={`mode-btn${mode === "video" ? " active" : ""}`}
                                    onClick={() => setMode("video")} disabled={downloading}>
                                <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" width="14"
                                     height="14">
                                    <path d="M15 10l4.553-2.07A1 1 0 0121 8.81v6.38a1 1 0 01-1.447.9L15 14"/>
                                    <rect x="3" y="6" width="12" height="12" rx="2"/>
                                </svg>
                                Видео
                            </button>
                            <button className={`mode-btn${mode === "audio" ? " active" : ""}`}
                                    onClick={() => setMode("audio")} disabled={downloading}>
                                <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" width="14"
                                     height="14">
                                    <path d="M9 18V5l12-2v13"/>
                                    <circle cx="6" cy="18" r="3"/>
                                    <circle cx="18" cy="16" r="3"/>
                                </svg>
                                Только аудио
                            </button>
                        </div>
                    </div>

                    {audioTracks.length > 1 && (
                        <div className="quality-section" style={{paddingTop: 0}}>
                            <p className="quality-label">Аудиодорожка</p>
                            <div className="quality-pills">
                                <button
                                    className={`quality-pill${audioLang === null ? " active" : ""}`}
                                    onClick={() => setAudioLang(null)} disabled={downloading}>
                                    Авто
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
                            <p className="quality-label">Кодек видео</p>
                            <div className="quality-pills">
                                <button className={`quality-pill${videoCodec === null ? " active" : ""}`}
                                        onClick={() => setVideoCodec(null)} disabled={downloading}>
                                    Авто
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
                            <p className="quality-label">Кодек аудио</p>
                            <div className="quality-pills">
                                <button className={`quality-pill${audioCodec === null ? " active" : ""}`}
                                        onClick={() => setAudioCodec(null)} disabled={downloading}>
                                    Авто
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
                            <p className="quality-label">Качество</p>
                            <div className="quality-pills">
                                {QUALITIES.map((q) => (
                                    <button key={q.value}
                                            className={`quality-pill${quality === q.value ? " active" : ""}`}
                                            onClick={() => setQuality(q.value)} disabled={downloading}>
                                        {q.label}
                                    </button>
                                ))}
                            </div>

                            <p style={{marginTop: "20px"}} className="quality-label">Фрагмент</p>
                            <div style={{display: "flex", gap: "8px"}}>
                                <input className="url-input" placeholder="00:00:00" value={startTime}
                                       onChange={(e) => setStartTime(e.target.value)}/>
                                <input className="url-input" value={endTime}
                                       onChange={(e) => setEndTime(e.target.value)}/>
                            </div>
                            {timeError && <p className="url-error">{timeError}</p>}

                            <p className="patch-label">Папка сохранения</p>
                            <button className="patch-pill" onClick={async () => {
                                const selected = await open({
                                    directory: true,
                                    multiple: false,
                                    defaultPath: downloadPath ?? undefined
                                });
                                if (typeof selected === "string") setDownloadPath(selected);
                            }}>
                                📁 {downloadPath ? downloadPath.split(/[\\/]/).filter(Boolean).pop() : "Загрузки (по умолчанию)"}
                            </button>
                        </div>
                    )}

                    {mode === "audio" && (
                        <div className="quality-section" style={{paddingTop: 0}}>
                            <p style={{marginTop: "1px"}} className="quality-label">Фрагмент</p>
                            <div style={{display: "flex", gap: "8px"}}>
                                <input className="url-input" placeholder="00:00:00" value={startTime}
                                       onChange={(e) => setStartTime(e.target.value)}/>
                                <input className="url-input" value={endTime}
                                       onChange={(e) => setEndTime(e.target.value)}/>
                            </div>
                            {timeError && <p className="url-error">{timeError}</p>}
                        </div>
                    )}

                    {error && (
                        <div className="error-box">
                            <div className="error-title">
                                <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" width="14"
                                     height="14">
                                    <circle cx="12" cy="12" r="10"/>
                                    <line x1="12" y1="8" x2="12" y2="12"/>
                                    <line x1="12" y1="16" x2="12.01" y2="16"/>
                                </svg>
                                Ошибка загрузки
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
                                Файл сохранён
                            </div>
                            <p className="result-path">{result.path}</p>
                            <p className="result-size">{result.file_size_mb.toFixed(1)} MB</p>
                        </div>
                    )}

                    <LogPanel logs={logs} downloading={downloading}/>

                    <div className="card-footer">
                        <button className="btn-primary" onClick={handleDownload} disabled={downloading}>
                            {singleDownloadLabel()}
                        </button>
                        {downloading && (
                            <button className="btn-cancel" onClick={handleCancel}>Отменить</button>
                        )}
                    </div>
                </div>
            )}

            {/* ── Playlist card ── */}
            {playlistInfo && urlKind === "playlist" && (
                <div className="card">
                    <div className="card-header">
                        <span className="badge badge-playlist">Плейлист</span>
                        <h2 className="card-title">{playlistInfo.title}</h2>
                        <p className="card-sub">
                            {playlistInfo.uploader && `${playlistInfo.uploader}  ·  `}
                            {playlistInfo.count} видео
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
                                <span>Треки плейлиста</span>
                            </span>
                            <span className="log-toggle-right">
                                <span className="log-count">{playlistInfo.entries.length} шт.</span>
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
                        <p className="quality-label">Формат</p>
                        <div className="mode-toggle">
                            <button className={`mode-btn${mode === "video" ? " active" : ""}`}
                                    onClick={() => setMode("video")} disabled={downloading}>
                                <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" width="14"
                                     height="14">
                                    <path d="M15 10l4.553-2.07A1 1 0 0121 8.81v6.38a1 1 0 01-1.447.9L15 14"/>
                                    <rect x="3" y="6" width="12" height="12" rx="2"/>
                                </svg>
                                Видео
                            </button>
                            <button className={`mode-btn${mode === "audio" ? " active" : ""}`}
                                    onClick={() => setMode("audio")} disabled={downloading}>
                                <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" width="14"
                                     height="14">
                                    <path d="M9 18V5l12-2v13"/>
                                    <circle cx="6" cy="18" r="3"/>
                                    <circle cx="18" cy="16" r="3"/>
                                </svg>
                                Только аудио
                            </button>
                        </div>
                    </div>

                    {audioTracks.length > 1 && (
                        <div className="quality-section" style={{paddingTop: 0}}>
                            <p className="quality-label">Аудиодорожка</p>
                            <div className="quality-pills">
                                <button
                                    className={`quality-pill${audioLang === null ? " active" : ""}`}
                                    onClick={() => setAudioLang(null)} disabled={downloading}>
                                    Авто
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
                            <p className="quality-label">Качество</p>
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
                        <p className="patch-label">Папка сохранения</p>
                        <button className="patch-pill" onClick={async () => {
                            const selected = await open({
                                directory: true,
                                multiple: false,
                                defaultPath: downloadPath ?? undefined
                            });
                            if (typeof selected === "string") setDownloadPath(selected);
                        }}>
                            📁 {downloadPath ? downloadPath.split(/[\\/]/).filter(Boolean).pop() : "Загрузки (по умолчанию)"}
                        </button>
                        <p className="path-hint">Плейлист будет сохранён в отдельную папку внутри выбранного
                            каталога</p>
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
                                Ошибка загрузки
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
                                Плейлист сохранён
                            </div>
                            <p className="result-path">{playlistResult.dir}</p>
                            <p className="result-size">
                                {playlistResult.downloaded} файлов · {playlistResult.total_size_mb.toFixed(1)} MB
                            </p>
                        </div>
                    )}

                    <LogPanel logs={logs} downloading={downloading}/>

                    <div className="card-footer">
                        <button className="btn-primary" onClick={handlePlaylistDownload} disabled={downloading}>
                            {playlistDownloadLabel()}
                        </button>
                        {downloading && (
                            <button className="btn-cancel" onClick={handleCancel}>Отменить</button>
                        )}
                    </div>
                </div>
            )}
        </main>
    );
}
