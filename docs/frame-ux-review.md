# Frame UX review

This is a heuristic task analysis based on the Frame design context, the v1 API contract, and the rendered interface. It is not interview research: no operator interviews or YouTube playback claims are represented here. Operator needs below are inferred from the product's single-operator workflow and safety constraints.

## Inferred operator task

An authorized operator wants to turn a permitted YouTube source into a file with a chosen media type, format, quality, and optional audio bitrate. They need to confirm the source before queueing work, understand whether the service is waiting, processing, or finished, and retrieve the authenticated output. If they change their mind or the service rejects a request, they need a clear way to stop or recover.

The current controls follow the v1 request contract: video offers MP4, WebM, or the source default plus best/worst or a height ceiling; audio offers MP3, M4A, or the M4A default, with best available quality and optional 32–320 kbps bitrate. Audio omits a worst-quality choice because the active adapter does not apply that setting.

| Step | Inferred need | Heuristic risk to check |
| --- | --- | --- |
| Enter a source and configure output | See the link and all choices before work begins; understand that source availability can limit quality | Invalid URLs, stale type-specific choices, or a bitrate outside 32–320 kbps could create a confusing late failure |
| Review the source | Verify the video in a native dialog and see the chosen output; leave without creating a job | A preview action could accidentally consume extraction capacity, or the dialog could trap keyboard users |
| Start and monitor work | Know that queued/running is not completion; see percentages only when the service reports them; cancel an active job | Decorative or estimated progress could imply a false completion time; a cancellation race could leave the UI polling a deleted job |
| Retrieve or recover | Use one authenticated file for preview and download; retain actionable errors and a path to retry | A malformed file URL could send credentials away from the same-origin route, or a stale object URL could retain an old file |

## Heuristic checks

- **Visibility of system status:** queued, running, completed, failed, and cancelled states should come from server responses. Show percentage only when it is finite and in range. Preserve output-panel geometry as states change so status does not jump around.
- **User control:** source review has explicit back and close actions; an active job has cancellation; completed output has a clear way to start another export; errors preserve retry or recovery actions.
- **Error prevention:** validate recognized YouTube links before opening the dialog; keep format, media type, quality, and bitrate in the request; create no job until the operator confirms; validate the returned file route against the current job ID before fetching it.
- **Match to the operator's language:** label format and quality choices in user-facing terms while sending the API's stable values. Explain that source availability can limit output quality and that the token is a private service credential.
- **Accessibility and resilience:** use native selects and dialog semantics, visible keyboard focus, readable light and dark themes, and a stacked narrow layout with no horizontal overflow. Honor reduced-motion preferences and tolerate invalid or unavailable theme storage.
- **Privacy:** hold the API token in tab memory, send it only in same-origin authorization headers, and avoid including tokens, source URLs, response bodies, or signed URLs in logs.

## Regression scenarios

| Scenario | Evidence expected | Evidence limit |
| --- | --- | --- |
| Review before export | Real browser shows the native confirmation dialog; no API request occurs until Continue | Confirms UI sequencing with local fixtures only |
| Configuration | Video and audio selections produce the corresponding type, format, quality, and optional bitrate fields | Confirms the browser payload, not stream availability or extraction success |
| Rejected access | A fixture `401` produces a persistent, safe error and no poll | Does not validate a deployed token or service |
| Cancellation | Confirmed job receives `DELETE`, pending poll is aborted, and no further status request is made | Does not establish worker cancellation timing |
| Returned file URL | Only the current job's exact relative file route is fetched; the same Blob backs preview and download; old Blob URLs are revoked | A generated Blob fixture does not prove codec support or low memory use |
| Keyboard and responsive layout | Enter opens review, Escape closes it without creating work, Enter confirms, and a narrow viewport stacks panels without horizontal overflow | Browser viewport checks do not cover every assistive technology or device |
| Theme handling | Valid light/dark choices toggle and persist; an invalid saved value falls back safely | Does not test every operating-system theme preference |

The offline browser workflow can be run with `node tests/frontend/browser.mjs` where Playwright and system Chromium are installed. The Node VM suite covers workflow, diagnostics, and security-sensitive edge cases. These fixtures establish local browser behavior only; they do not establish YouTube access, playback, deployed authentication, remote-worker behavior, or Railway reliability.
