export interface UploadProgress { loaded: number; total: number }

/** Same-origin upload with byte progress; fetch does not expose upload progress events. */
export function uploadRequest(url: string, body: Blob, options: {
  method: string;
  headers: Record<string, string>;
  signal?: AbortSignal;
  onProgress: (progress: UploadProgress) => void;
}): Promise<Response> {
  return new Promise((resolve, reject) => {
    const xhr = new XMLHttpRequest();
    const aborted = () => new DOMException("The upload was cancelled.", "AbortError");
    if (options.signal?.aborted) { reject(aborted()); return; }
    const abort = () => xhr.abort();
    const cleanup = () => options.signal?.removeEventListener("abort", abort);
    xhr.open(options.method, url);
    for (const [name, value] of Object.entries(options.headers)) xhr.setRequestHeader(name, value);
    xhr.upload.onprogress = event => options.onProgress({ loaded: event.loaded, total: event.lengthComputable ? event.total : body.size });
    xhr.upload.onload = () => options.onProgress({ loaded: body.size, total: body.size });
    xhr.onload = () => {
      cleanup();
      const headers = new Headers();
      for (const line of xhr.getAllResponseHeaders().trim().split(/[\r\n]+/)) {
        const colon = line.indexOf(":");
        if (colon > 0) headers.append(line.slice(0, colon), line.slice(colon + 1).trim());
      }
      resolve(new Response(xhr.status === 204 || xhr.status === 205 ? null : xhr.responseText, { status: xhr.status, headers }));
    };
    xhr.onerror = () => { cleanup(); reject(new TypeError("The upload connection failed")); };
    xhr.onabort = () => { cleanup(); reject(aborted()); };
    options.signal?.addEventListener("abort", abort, { once: true });
    xhr.send(body);
  });
}
