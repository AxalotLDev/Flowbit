import "./App.css";
import { useState, useEffect, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen, UnlistenFn } from "@tauri-apps/api/event";

// ──────────────────────────────────────────────
// Types
// ──────────────────────────────────────────────

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

interface VideoInfo {
  title: string;
  author_name: string;
  html: string;
  [key: string]: unknown;
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

// ──────────────────────────────────────────────
// Component
// ──────────────────────────────────────────────

export default function App() {
  const [url, setUrl] = useState("");
  const [video, setVideo] = useState<VideoInfo | null>(null);
  const [urlError, setUrlError] = useState("");
  const [downloadError, setDownloadError] = useState<DownloadError | null>(null);
  const [downloading, setDownloading] = useState(false);
  const [done, setDone] = useState(false);
  const [result, setResult] = useState<DownloadResult | null>(null);
  const [progress, setProgress] = useState<DownloadProgress>({
    percent: 0,
    speed: "—",
    downloaded_bytes: 0,
    total_bytes: 0,
  });

  // ── Event listeners ──────────────────────────
  useEffect(() => {
    let unlistenProgress: UnlistenFn | undefined;
    let unlistenError: UnlistenFn | undefined;

    listen<DownloadProgress>("download-progress", (event) => {
      const p = event.payload;
      setProgress(p);
      setDownloadError(null);
      if (p.percent >= 100) {
        setDone(true);
        setDownloading(false);
      }
    }).then((fn) => { unlistenProgress = fn; });

    listen<string>("download-error", (event) => {
      const msg = event.payload;
      setDownloadError({ message: msg, hint: classifyError(msg) });
      setDownloading(false);
    }).then((fn) => { unlistenError = fn; });

    return () => {
      unlistenProgress?.();
      unlistenError?.();
    };
  }, []);

  // ── URL validation + video info ───────────────
  useEffect(() => {
    if (!url.trim()) {
      setVideo(null);
      setUrlError("");
      return;
    }

    const timeout = setTimeout(async () => {
      setUrlError("");
      setVideo(null);
      setDone(false);
      setDownloadError(null);
      setResult(null);

      try {
        const valid = await invoke<boolean>("is_youtube_url", { text: url });
        if (!valid) {
          setUrlError("Неверный YouTube URL");
          return;
        }
        const info = await invoke<VideoInfo>("get_youtube_info", { url });
        setVideo(info);
      } catch (e) {
        console.error("[get_youtube_info]", e);
        setUrlError("Не удалось получить информацию о видео");
      }
    }, 600);

    return () => clearTimeout(timeout);
  }, [url]);

  // ── Download handler ──────────────────────────
  const handleDownload = useCallback(async () => {
    if (!url || downloading) return;

    setDownloading(true);
    setDone(false);
    setResult(null);
    setDownloadError(null);
    setProgress({ percent: 0, speed: "—", downloaded_bytes: 0, total_bytes: 0 });

    try {
      // path is omitted → backend picks the Downloads folder automatically
      const res = await invoke<DownloadResult>("download_video", { url });
      setResult(res);
    } catch (err) {
      const msg =
        err instanceof Error
          ? err.message
          : typeof err === "string"
          ? err
          : JSON.stringify(err);
      setDownloadError({ message: msg, hint: classifyError(msg) });
    } finally {
      setDownloading(false);
    }
  }, [url, downloading]);

  // ── Cancel handler ────────────────────────────
  const handleCancel = useCallback(async () => {
    if (!url) return;
    try {
      await invoke("cancel_download", { url });
    } catch (e) {
      console.error("[cancel_download]", e);
    }
  }, [url]);

  // ──────────────────────────────────────────────
  // Render
  // ──────────────────────────────────────────────
  return (
    <main className="container">
      <input
        placeholder="Вставьте YouTube URL"
        type="text"
        value={url}
        onChange={(e) => setUrl(e.target.value)}
        disabled={downloading}
      />

      <hr />

      {/* URL validation error */}
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

      {/* Video card */}
      {video && (
        <div className="video-info">
          <h3>{video.title}</h3>
          <p className="video-author">Автор: {video.author_name}</p>

          <div className="video-wrapper">
            <div dangerouslySetInnerHTML={{ __html: video.html }} />
          </div>

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

                {done ? (
                  <span className="done-label">✓ Готово</span>
                ) : (
                  <span className="status-label">Загрузка…</span>
                )}
              </div>
            </div>
          )}

          {/* Saved path */}
          {result && (
            <div className="saved-path">
              <p>
                ✅ Файл сохранён:{" "}
                <strong title={result.path}>{result.path}</strong>
              </p>
              <p className="file-size">Размер: {formatBytes(result.file_size_bytes)}</p>
            </div>
          )}

          {/* Action buttons */}
          <div className="button-row">
            <button onClick={handleDownload} disabled={downloading}>
              {downloading ? "Загружается…" : "Скачать"}
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