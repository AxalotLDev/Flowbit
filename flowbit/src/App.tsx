import "./App.css";
import {useState, useEffect, useCallback, useRef} from "react";
import {invoke} from "@tauri-apps/api/core";
import {openUrl} from "@tauri-apps/plugin-opener";
import {open} from "@tauri-apps/plugin-dialog";

type Platform = "youtube" | "twitch" | null;
type Quality = "best" | "high" | "medium" | "low" | "worst";
type DownloadMode = "video" | "audio";

interface YoutubeInfo {
    title: string;
    author_name: string;
    html: string;
}

interface TwitchInfo {
    title: string;
    channel: string;
    duration: number | null;
    is_live: boolean;
    thumbnail_url: string | null;
}

interface DownloadResult {
    path: string;
    file_size_mb: number;
}

function formatDuration(s: number) {
    const h = Math.floor(s / 3600), m = Math.floor((s % 3600) / 60), sec = s % 60;
    return h > 0
        ? `${h}:${String(m).padStart(2, "0")}:${String(sec).padStart(2, "0")}`
        : `${m}:${String(sec).padStart(2, "0")}`;
}

const QUALITIES: { value: Quality; label: string }[] = [
    {value: "best", label: "Лучшее"},
    {value: "high", label: "1080p"},
    {value: "medium", label: "720p"},
    {value: "low", label: "480p"},
    {value: "worst", label: "Худшее"},
];

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

function extractYouTubeId(url: string): string | null {
    try {
        const u = new URL(url);

        if (u.hostname.includes("youtu.be")) {
            return u.pathname.slice(1);
        }

        if (u.hostname.includes("youtube.com")) {
            return u.searchParams.get("v");
        }

        return null;
    } catch {
        return null;
    }
}

export default function App() {
    const [url, setUrl] = useState("");
    const [platform, setPlatform] = useState<Platform>(null);
    const [youtubeInfo, setYoutubeInfo] = useState<YoutubeInfo | null>(null);
    const [twitchInfo, setTwitchInfo] = useState<TwitchInfo | null>(null);
    const [quality, setQuality] = useState<Quality>("best");
    const [mode, setMode] = useState<DownloadMode>("video");
    const [urlError, setUrlError] = useState("");
    const [downloading, setDownloading] = useState(false);
    const [result, setResult] = useState<DownloadResult | null>(null);
    const [error, setError] = useState("");
    const [loadingInfo, setLoadingInfo] = useState(false);
    const [downloadPath, setDownloadPath] = useState<string | null>(null);
    const abortRef = useRef<AbortController | null>(null);

    useEffect(() => {
        abortRef.current?.abort();

        if (!url.trim()) {
            resetAll();
            return;
        }

        setYoutubeInfo(null);
        setTwitchInfo(null);
        setPlatform(null);
        setUrlError("");
        setResult(null);
        setError("");

        const controller = new AbortController();
        abortRef.current = controller;

        const timer = setTimeout(async () => {
            setLoadingInfo(true);

            try {
                const [isYt, isTw] = await Promise.all([
                    invoke<boolean>("is_youtube_url", {url: url}),
                    invoke<boolean>("is_twitch_url", {url: url}),
                ]);

                if (controller.signal.aborted) return;

                if (isYt) {
                    setPlatform("youtube");
                    const info = await invoke<YoutubeInfo>("get_youtube_info", {url});
                    if (!controller.signal.aborted) setYoutubeInfo(info);
                } else if (isTw) {
                    setPlatform("twitch");
                    const info = await invoke<TwitchInfo>("get_twitch_info", {url});
                    if (!controller.signal.aborted) setTwitchInfo(info);
                } else {
                    if (!controller.signal.aborted)
                        setUrlError(" Неверный URL — поддерживаются YouTube и Twitch");
                }
            } catch {
                if (!controller.signal.aborted)
                    setUrlError(" Не удалось получить информацию о видео");
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
        setPlatform(null);
        setUrlError("");
        setResult(null);
        setError("");
        setLoadingInfo(false);
    }

    const handleDownload = useCallback(async () => {
        if (!platform || downloading) return;
        setDownloading(true);
        setResult(null);
        setError("");
        try {
            const cmd = platform === "twitch" ? "download_twitch" : "download_video";
            const res = await invoke<DownloadResult>(cmd, {url, quality, mode, path: downloadPath ?? undefined});
            setResult(res);
        } catch (e) {
            setError(typeof e === "string" ? e : String(e));
        } finally {
            setDownloading(false);
        }
    }, [url, platform, quality, mode, downloading]);

    const hasInfo = youtubeInfo !== null || twitchInfo !== null;
    const title = youtubeInfo?.title ?? twitchInfo?.title ?? "";
    const sub = youtubeInfo
        ? `Автор: ${youtubeInfo.author_name}`
        : twitchInfo
            ? [
                twitchInfo.channel,
                twitchInfo.is_live ? "🔴 Live" : "",
                twitchInfo.duration ? formatDuration(twitchInfo.duration) : "",
            ].filter(Boolean).join("  ·  ")
            : "";

    const downloadLabel = () => {
        if (downloading) return <><span className="spinner"/>Загружается…</>;
        if (twitchInfo?.is_live) return mode === "audio" ? "Записать аудио" : "Записать стрим";
        return mode === "audio" ? "Скачать аудио" : "Скачать видео";
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
                    onChange={e => setUrl(e.target.value)}
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

            {!hasInfo && !urlError && !loadingInfo && (
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

            {hasInfo && (
                <div className="card">
                    <div className="card-header">
            <span className={`badge badge-${platform}`}>
              {platform === "twitch" ? "Twitch" : "YouTube"}
            </span>
                        <h2 className="card-title">{title}</h2>
                        <p className="card-sub">{sub}</p>
                    </div>

                    {youtubeInfo && (() => {
                        const videoId = extractYouTubeId(url);

                        if (!videoId) return null;

                        return (
                            <div className="embed-wrap">
                                <iframe
                                    src={`https://www.youtube.com/embed/${videoId}`}
                                    allowFullScreen
                                />

                                <button
                                    className="btn-primary btn-primary-link"
                                    onClick={async () => await openUrl(url)}
                                >
                                    Открыть в YouTube
                                </button>
                            </div>
                        );
                    })()}
                    {twitchInfo?.thumbnail_url && (
                        <div className="thumb-wrap">
                            <img src={twitchInfo.thumbnail_url} alt={twitchInfo.title}/>
                            <button
                                className="btn-primary btn-primary-link"
                                onClick={async () => await openUrl(url)}
                            >
                                Открыть в Twitch
                            </button>
                        </div>
                    )}

                    <div className="divider"/>

                    <div className="quality-section">
                        <p className="quality-label">Формат</p>
                        <div className="mode-toggle">
                            <button
                                className={`mode-btn${mode === "video" ? " active" : ""}`}
                                onClick={() => setMode("video")}
                                disabled={downloading}
                            >
                                <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" width="14"
                                     height="14">
                                    <path d="M15 10l4.553-2.07A1 1 0 0121 8.81v6.38a1 1 0 01-1.447.9L15 14"/>
                                    <rect x="3" y="6" width="12" height="12" rx="2"/>
                                </svg>
                                Видео
                            </button>
                            <button
                                className={`mode-btn${mode === "audio" ? " active" : ""}`}
                                onClick={() => setMode("audio")}
                                disabled={downloading}
                            >
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

                    {mode === "video" && (
                        <div className="quality-section" style={{paddingTop: 0}}>
                            <p className="quality-label">Качество</p>
                            <div className="quality-pills">
                                {QUALITIES.map(q => (
                                    <button
                                        key={q.value}
                                        className={`quality-pill${quality === q.value ? " active" : ""}`}
                                        onClick={() => setQuality(q.value)}
                                        disabled={downloading}
                                    >{q.label}</button>
                                ))}
                            </div>

                            <p className="patch-label">Папка сохранения</p>

                            <button
                                className="patch-pill"
                                onClick={async () => {
                                    const selected = await open({
                                        directory: true,
                                        multiple: false,
                                        defaultPath: downloadPath ?? undefined,
                                    });

                                    if (typeof selected === "string") {
                                        setDownloadPath(selected);
                                    }
                                }}
                            >
                                📁 {downloadPath ? downloadPath.split("/").pop() : "Downloads (по умолчанию)"}
                            </button>
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
                                <a> Ошибка загрузки</a>
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

                    <div className="card-footer">
                        <button
                            className="btn-primary"
                            onClick={handleDownload}
                            disabled={downloading}
                        >
                            {downloadLabel()}
                        </button>
                    </div>
                </div>
            )}
        </main>
    );
}