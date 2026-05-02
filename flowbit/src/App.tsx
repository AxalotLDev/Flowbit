import "./App.css";
import { useState, useEffect, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen, UnlistenFn } from "@tauri-apps/api/event";

// ──────────────────────────────────────────────
// Types
// ──────────────────────────────────────────────

type Platform = "youtube" | "twitch" | null;

interface DownloadProgress {
  percent: number;
  speed: string;
  downloaded_bytes: number;
  total_bytes: number;
}

interface DownloadResult {
  path: string;
  file_size_bytes: number;
}

interface YoutubeInfo {
  title: string;
  author_name: string;
  html: string;
  [key: string]: unknown;
}

interface TwitchInfo {
  title: string;
  channel: string;
  duration: number | null;
  is_live: boolean;
  thumbnail_url: string | null;
  view_count: number | null;
}

interface DownloadError {
  message: string;
  hint: string;
}

// ──────────────────────────────────────────────
// Helpers
// ──────────────────────────────────────────────

function classifyError(msg: string): string {
  if (msg.includes("ffmpeg") || msg.includes("FFmpeg"))
    return "FFmpeg не найден или неправильно настроен. Проверьте путь libs/ffmpeg.";
  if (msg.includes("yt-dlp"))
    return "Проблема с yt-dlp. Проверьте бинарный файл libs/yt-dlp.";
  if (msg.includes("permission") || msg.includes("Permission"))
    return "Нет прав на запись в папку загрузок.";
  if (msg.includes("disk") || msg.includes("space") || msg.includes("No space"))
    return "Ошибка диска. Проверьте свободное место.";
  if (msg.includes("network") || msg.includes("connection") || msg.includes("timeout"))
    return "Проблема с сетью. Проверьте интернет-соединение.";
  if (msg.includes("subscriber") || msg.includes("expired"))
    return "VOD недоступен — возможно, только для подписчиков или удалён.";
  if (msg.includes("private") || msg.includes("unavailable"))
    return "Видео недоступно или является приватным.";
  if (msg.includes("cancelled") || msg.includes("canceled"))
    return "Загрузка была отменена пользователем.";
  return "Проверьте логи приложения для получения подробностей.";
}

function formatBytes(bytes: number): string {
  if (bytes === 0) return "—";
  if (bytes >= 1024 ** 3) return `${(bytes / 1024 ** 3).toFixed(2)} GB`;
  if (bytes >= 1024 ** 2) return `${(bytes / 1024 ** 2).toFixed(1)} MB`;
  if (bytes >= 1024) return `${(bytes / 1024).toFixed(0)} KB`;
  return `${bytes} B`;
}

function formatDuration(seconds: number): string {
  const h = Math.floor(seconds / 3600);
  const m = Math.floor((seconds % 3600) / 60);
  const s = seconds % 60;
  if (h > 0) return `${h}:${String(m).padStart(2, "0")}:${String(s).padStart(2, "0")}`;
  return `${m}:${String(s).padStart(2, "0")}`;
}

// ──────────────────────────────────────────────
// Component
// ──────────────────────────────────────────────

export default function App() {
  const [url, setUrl]                         = useState("");
  const [platform, setPlatform]               = useState<Platform>(null);

  // Info
  const [youtubeInfo, setYoutubeInfo]         = useState<YoutubeInfo | null>(null);
  const [twitchInfo, setTwitchInfo]           = useState<TwitchInfo | null>(null);

  // Status
  const [urlError, setUrlError]               = useState("");
  const [downloadError, setDownloadError]     = useState<DownloadError | null>(null);
  const [downloading, setDownloading]         = useState(false);
  const [done, setDone]                       = useState(false);
  const [result, setResult]                   = useState<DownloadResult | null>(null);
  const [progress, setProgress]               = useState<DownloadProgress>({
    percent: 0, speed: "—", downloaded_bytes: 0, total_bytes: 0,
  });

  // ── Event listeners ──────────────────────────
  useEffect(() => {
    const unsubs: UnlistenFn[] = [];

    // YouTube events
    listen<DownloadProgress>("download-progress", (e) => {
      setProgress(e.payload);
      setDownloadError(null);
      if (e.payload.percent >= 100) { setDone(true); setDownloading(false); }
    }).then((fn) => unsubs.push(fn));

    listen<string>("download-error", (e) => {
      setDownloadError({ message: e.payload, hint: classifyError(e.payload) });
      setDownloading(false);
    }).then((fn) => unsubs.push(fn));

    // Twitch events
    listen<DownloadProgress>("twitch-progress", (e) => {
      setProgress(e.payload);
      setDownloadError(null);
      if (e.payload.percent >= 100) { setDone(true); setDownloading(false); }
    }).then((fn) => unsubs.push(fn));

    listen<{ message: string; error_type: string }>("twitch-error", (e) => {
      const msg = e.payload.message;
      setDownloadError({ message: msg, hint: classifyError(msg) });
      setDownloading(false);
    }).then((fn) => unsubs.push(fn));

    return () => unsubs.forEach((fn) => fn());
  }, []);

  // ── URL detection + info fetch ────────────────
  useEffect(() => {
    if (!url.trim()) {
      resetInfo();
      return;
    }

    const timer = setTimeout(async () => {
      resetInfo();

      try {
        // Detect platform first — two parallel checks
        const [isYt, isTw] = await Promise.all([
          invoke<boolean>("is_youtube_url", { text: url }),
          invoke<boolean>("is_twitch_url",  { text: url }),
        ]);

        if (isYt) {
          setPlatform("youtube");
          const info = await invoke<YoutubeInfo>("get_youtube_info", { url });
          setYoutubeInfo(info);
        } else if (isTw) {
          setPlatform("twitch");
          const info = await invoke<TwitchInfo>("get_twitch_info", { url });
          setTwitchInfo(info);
        } else {
          setUrlError("Неверный URL — поддерживаются YouTube и Twitch");
        }
      } catch (e) {
        console.error("[get_info]", e);
        setUrlError("Не удалось получить информацию о видео");
      }
    }, 600);

    return () => clearTimeout(timer);
  }, [url]);

  function resetInfo() {
    setYoutubeInfo(null);
    setTwitchInfo(null);
    setPlatform(null);
    setUrlError("");
    setDone(false);
    setDownloadError(null);
    setResult(null);
  }

  // ── Download ──────────────────────────────────
  const handleDownload = useCallback(async () => {
    if (!url || downloading || !platform) return;

    setDownloading(true);
    setDone(false);
    setResult(null);
    setDownloadError(null);
    setProgress({ percent: 0, speed: "—", downloaded_bytes: 0, total_bytes: 0 });

    try {
      const command = platform === "twitch" ? "download_twitch" : "download_video";
      const res = await invoke<DownloadResult>(command, { url });
      setResult(res);
    } catch (err) {
      const msg = err instanceof Error ? err.message
          : typeof err === "string" ? err
              : JSON.stringify(err);
      setDownloadError({ message: msg, hint: classifyError(msg) });
    } finally {
      setDownloading(false);
    }
  }, [url, downloading, platform]);

  // ── Cancel ────────────────────────────────────
  const handleCancel = useCallback(async () => {
    if (!url || !platform) return;
    const command = platform === "twitch" ? "cancel_twitch_download" : "cancel_download";
    await invoke(command, { url }).catch(console.error);
  }, [url, platform]);

  // ──────────────────────────────────────────────
  // Derived
  // ──────────────────────────────────────────────
  const hasInfo  = youtubeInfo !== null || twitchInfo !== null;
  const title    = youtubeInfo?.title    ?? twitchInfo?.title    ?? "";
  const subtitle = youtubeInfo
      ? `Автор: ${youtubeInfo.author_name}`
      : twitchInfo
          ? `${twitchInfo.channel}${twitchInfo.is_live ? " 🔴 Live" : ""}${twitchInfo.duration ? "  ·  " + formatDuration(twitchInfo.duration) : ""}`
          : "";

  // ──────────────────────────────────────────────
  // Render
  // ──────────────────────────────────────────────
  return (
      <main className="container">
        <input
            placeholder="Вставьте YouTube или Twitch URL"
            type="text"
            value={url}
            onChange={(e) => setUrl(e.target.value)}
            disabled={downloading}
        />

        <hr />

        {/* URL error */}
        {urlError && <p className="error-message">{urlError}</p>}

        {/* Download error */}
        {downloadError && (
            <div className="download-error">
              <p className="error-title">❌ Ошибка загрузки</p>
              <p className="error-message">{downloadError.message}</p>
              <details className="error-details">
                <summary>Подробнее</summary>
                <p>{downloadError.hint}</p>
                <ul className="error-help">
                  <li>Проверьте интернет-соединение</li>
                  <li>Убедитесь, что видео общедоступно</li>
                  <li>Проверьте свободное место на диске</li>
                  <li>Перезапустите приложение</li>
                </ul>
              </details>
            </div>
        )}

        {/* Info card */}
        {hasInfo && (
            <div className="video-info">
              {/* Platform badge */}
              <span className={`platform-badge platform-badge--${platform}`}>
            {platform === "twitch" ? "Twitch" : "YouTube"}
          </span>

              <h3>{title}</h3>
              <p className="video-author">{subtitle}</p>

              {/* YouTube embed */}
              {youtubeInfo && (
                  <div className="video-wrapper">
                    <div dangerouslySetInnerHTML={{ __html: youtubeInfo.html }} />
                  </div>
              )}

              {/* Twitch thumbnail */}
              {twitchInfo?.thumbnail_url && (
                  <div className="video-wrapper">
                    <img
                        src={twitchInfo.thumbnail_url}
                        alt={twitchInfo.title}
                        className="twitch-thumbnail"
                    />
                  </div>
              )}

              {/* Progress */}
              {(downloading || done) && (
                  <div className="progress-wrapper">
                    <div className="progress-bar-track">
                      <div
                          className="progress-bar-fill"
                          style={{ width: `${progress.percent}%` }}
                      />
                    </div>
                    <div className="progress-meta">
                      <span>{Math.round(progress.percent)}%</span>
                      {progress.total_bytes > 0 ? (
                          <span>
                    {formatBytes(progress.downloaded_bytes)} / {formatBytes(progress.total_bytes)}
                  </span>
                      ) : (
                          <span>{progress.speed}</span>
                      )}
                      {done
                          ? <span className="done-label">✓ Готово</span>
                          : <span className="status-label">Загрузка…</span>}
                    </div>
                  </div>
              )}

              {/* Result */}
              {result && (
                  <div className="saved-path">
                    <p>✅ Файл сохранён: <strong title={result.path}>{result.path}</strong></p>
                    <p className="file-size">Размер: {formatBytes(result.file_size_bytes)}</p>
                  </div>
              )}

              {/* Buttons */}
              <div className="button-row">
                <button onClick={handleDownload} disabled={downloading}>
                  {downloading
                      ? "Загружается…"
                      : twitchInfo?.is_live
                          ? "Записать стрим"
                          : "Скачать"}
                </button>
                {downloading && (
                    <button className="cancel-btn" onClick={handleCancel}>
                      Отмена
                    </button>
                )}
              </div>
            </div>
        )}
      </main>
  );
}