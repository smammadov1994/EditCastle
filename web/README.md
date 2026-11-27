# EditCastle Director for the web

The Rust server hosts the scrolling EditCastle site and a private video editor. It keeps your projects, footage, and credits on its host. Connect an OpenAI or Anthropic API key from the Director panel. The provider plans edits and reviews sampled stills from generated shots. Pi-H3 on your ComfyUI GPU renders the video.

## Start locally

With Rust, FFmpeg, FFprobe, and the GPU's private forwarded port available, run:

```sh
./start-editcastle.sh --open
```

Use `./start-editcastle.sh` on Linux or when running as a service. The default address is `http://127.0.0.1:4324`. In standalone mode, the first launch creates the studio access key in `studio-data/access-key` with owner-only permissions. Open `/studio` and enter that key to sign in from another browser. The macOS launcher can sign in locally through a browser fragment that the page immediately clears. With Supabase configured, the launcher opens email sign-in. Director's model API keys stay in the server's session memory. Re-enter them after signing out, after the session expires, or after restarting the server.

In Director settings, choose the editing model and its supported effort. ChatGPT and Claude retain separate settings and keys within the studio session. Connected tabs switch providers without asking for the key again. Disconnect a provider in settings to remove its key from the session. The editing model plans and reviews shots. The video model setting chooses the ComfyUI renderer used on the GPU.

An idea submitted from Studio Home opens in Director. If you press **Send** before choosing a planner, **Connect and send** submits that draft after connecting. Each project keeps its unsent prompt in the current browser tab through reloads and sign-in. Studio clears the submitted text once it accepts the request.

Choose **Test connection** in Director settings to check whether the selected model is visible to a new key or the key already connected in this session. The check requests model metadata from the provider and leaves your saved settings and generation credits in place.

Set `EDITROOM_DATA` to change the project storage directory. Back up that directory to preserve projects, media, the credit ledger, and the owner key. The web studio has its own projects and credits. Import video and audio from an existing Mac project if you want to use that footage on the web.

The `editcastle-web` binary includes the landing page, editor, fonts, and demo media. You can run it outside the source checkout. Set `EDITROOM_HOME_ASSETS` to serve your own copies of the Studio Home images and videos from a directory.

## Organize media folders

The Media panel can upload a folder from your browser and preserve its subfolders. In a browser with the File System Access API, **Link a folder on this device** keeps that folder in sync while the signed-in editor is open. It requests read access to the chosen folder. The browser saves the folder handle locally and sends only project-relative media paths and copied media to the studio. Use **Sync this device** if the browser asks to restore access. Exact-content renames keep the original asset ID. Missing files retain a playable copy, and ambiguous duplicates remain distinct. To follow changes over time, link a folder in the studio inbox. The inbox defaults to `studio-data/inbox` or can be set with `EDITROOM_MEDIA_INBOX`. EditCastle scans linked folders on the host, brings in new media, and follows renames and moves within the inbox. It keeps a private copy available for playback if the original goes offline. The folder actions can scan, locate, reload, or remove a project folder. Changed inbox files reload after their size and timestamp settle. Use **Reload from Disk** on an inbox source or **Replace file** on an uploaded source. The editor keeps source IDs, trims clips that exceed a shorter replacement, and makes the change undoable. Exports already underway keep the source versions they started with. Removing a folder and its timeline uses is undoable. Whole-library and folder searches include nested media.

Director can use synced device media as soon as it appears in the project. Director can discover inbox paths with `list_media_folders`, link them with `import_media`, and scan them with `sync_media_folders`. `list_media` includes each item's folder label. The MCP `reload_media` tool refreshes an inbox source or folder in the saved project. Imports from a native `.room` retain the library's folder layout.

For an offline inbox source, choose **Locate in Inbox…** and select its new file. EditCastle also looks in that folder for other missing sources with matching filenames. The cut keeps their source IDs and timeline positions. In Media, select multiple inbox sources and choose **Reload Items from Disk** to refresh them together. One Undo restores the previous media and cut. MCP can reload a selection by passing `asset_ids` to `reload_media`.

Drop a whole folder from Finder into **Media** or onto the timeline. EditCastle preserves subfolders and reads supported files in name order. Browsers that grant ongoing directory access link the folder for sync. Other browsers that provide its contents import a copy. Dropping a linked subfolder reuses its media. Dropping its parent moves those existing entries under the new root and preserves their clip references.

Double-click a folder to open it. Click once to select it, or use Shift or Command to pick folders and sources together. **Remove from project** removes the selected library items and the clips that use them. EditCastle asks for confirmation when the cut or soundtrack uses the media. The original files stay in place, and one Undo restores the media, folder structure, and cut. Sources taken out of a linked folder stay out during sync. Authenticated MCP clients can preview and apply the same operation through `remove_media`.

Move the pointer across a video thumbnail in Media to scrub the source. The thumbnail and main viewer show the same frame with a source time readout. Move away to return to the cut or the source you opened previously. This works in both the thumbnail grid and compact list.

Choose **Download Source** from the context menu in Media or on the timeline to save the original file. A shot in Director's conversation also has its own download button. Generated shots and older sources with only a studio copy download that saved copy.

## Render a cut

Open **File → Editor Settings** to choose a Mac editor theme, switch between media thumbnails and the list, or display time in frames or in minutes and seconds. The browser remembers your preferences. **Auto** follows the device's appearance.

On desktop, drag the divider between Media and the viewer to resize Media, or the divider above the timeline to resize the timeline. Focus either divider and use the arrow keys to resize it. Press **Enter** or double-click to restore its default size. The browser remembers the sizes on this device.

**Auto-save projects** is on by default. Turn it off under **Saving** to keep the last saved version of each project while you edit. **File → Save Project** or **Command+S** saves the current project, and **Revert to Saved** restores its last saved edits and media library. Your working copy survives a browser reload or studio restart. Turning on auto-save saves all working projects. **Download Project** creates a portable copy of the current working project. Attached MCP clients can save the project's working copy with `save_project`.

**File → Save As** opens a separately editable copy of the working cut and library. Both projects use the existing media files and can edit and replace their sources independently. Device folders keep their links on the browser that granted folder access. **File → Open Recent** lists projects recently opened or saved in this browser. **Clear Menu** clears the list.

Under **File → Editor Settings → Director**, turn off **Enable Director** to hide the panel and its toolbar button. Submitting a film idea from Studio Home enables it again and opens your draft. You can also choose whether to watch draft edits on the timeline. With draft viewing turned off, render and review progress still appears in Activity and the finished cut appears as one undoable edit.

Open a Mac `.room` project to bring its recent prompts and Quick Clips conversations into Director. Expand a reply's shot plan or run activity, or click an attached shot to select it on the timeline or in Media. Choose **History** above the prompt or press **Up** in an empty prompt box to bring back a recent prompt. **Ideas** fills in an example you can edit before sending. **Save As** and downloaded projects carry the conversation with them. Older Mac failures show their status in the browser. The MCP `studio_video_status` tool reports saved Quick Clips results from the current project.

Choose **Auto Edit** in the Director header for an edit using your local Media. With no picture clips on the timeline, it scores footage for detail, exposure, colour, movement, and available face and aesthetics measurements. It orders sources by their capture date when present, builds a rough cut, and trims and moves the strongest moments. When it detects music, it aligns cuts to beats and brings the footage audio under the song. With picture clips already on the timeline, it refines the existing shots and keeps their order. The draft shows the actual placements, trims, and moves. **Stop** discards the draft, and one Undo restores the previous cut after it saves. Attached clients can run the same workflow with MCP `auto_edit`.

Drop video, audio, and still files onto the studio to import them into **Media**, or onto timeline lanes to place them end to end. The empty viewer also offers **Import Media**. Drop onto the empty **+ Video** or **+ Audio** lane to create the needed track. A video and its sound land together on paired tracks. When the target is locked, EditCastle uses an unlocked track or makes one. Dropped clips overwrite existing material in their range. Undo restores the previous cut and leaves the imported files in Media. Shift-click or Command-click sources in Media and drag the selected group to move them onto the timeline together. Director can do the same with MCP's `place_media` command.

Double-click a source in Media or choose **Add to Timeline** to place it at the playhead. EditCastle uses free, unlocked tracks or adds tracks so the existing cut stays in place. MCP's `add_clip` supports the same placement with `first_free_track`.

Enter a prompt on the home page or in Director. You can specify a sequence, an exact shot duration, or a change to the current timeline. To continue from existing footage, select the relevant clip and ask Director to continue it. ComfyUI starts from the last visible frame of that clip as edited, including timing, reframing, fades, and grade. That frame contains only the selected clip. Director reviews each take before placing it. You can trim, split, arrange, mute, add fades, add a soundtrack, and export MP4.

Use **Seconds per shot** to set every generated shot's length in Director or Quick Clips. The GPU renders on its 24 fps grid, and Studio trims each accepted render to the requested length. In Quick Clips, a conflicting shot length and planned runtime stops the run before credits are reserved.

Shift-click or drag a selection box over clips to select a group. Drag the group to move it, or hold Option to drop a copy. Linked picture and sound keep their offsets. Drag a clip edge to trim the same edge of the group. **Sticky** carries touching neighbours. With the timeline focused, use the standard select-all, cut, copy, and paste shortcuts. Paste starts at the playhead. Use **Command+Shift+D** to duplicate, **Command+D** for a dissolve at the selected cut nearest the playhead, **Command+K** to split, and **Q** or **W** to ripple trim to the playhead. **Command+Shift+L** detaches linked audio from the selection. **Option+F** freezes the selected frame, and **Option+R** reverses the selected source and its linked audio. **Command+T** inserts a default title at the first open spot. **Shift+Left** and **Shift+Right** jump to edit points, and **Home** and **End** jump to the cut boundaries. The browser also accepts Control for Command shortcuts. Director can use the same timeline operations through MCP.

Open **File → Keyboard Shortcuts** or press **Command+,** to search and change the default keys. Assigning a used combination moves it to the new command. Clear a binding, reset an individual command, or restore all shortcut defaults. The browser saves your preferences on this device. The Playback section also lets you choose whether playback starts from the skimmer or the playhead.

The toolbar's **Clip** and **Mark** menus offer the Mac editor's commands and show your current shortcut assignments. Open a menu with the down arrow from its toolbar button. Use the arrow keys to move between available commands, **Enter** to run one, and **Escape** to return to the toolbar.

Right-click a video or audio clip for the Mac speed presets. The Speed menu shows the current preset and applies your choice to the selected clips. Picture clips also have grade copy and paste commands. Use **Right** to enter a submenu and **Left** to return. Right-click a caption to edit its text, remove captions from its clip, or show, hide, or delete the caption track. Those track commands are also available from empty space on the caption lane.

The caption lane shows speech in short phrases that follow the clip through moves, trims, copies, and splits. Double-click a caption to correct it. The correction follows every use of that source. Select multiple captions to omit their words without editing the audio. The C1 track header opens shared size, position, text color, highlight, and outline controls. Toggle its eye to hide captions in the viewer and export. Portrait and square exports lay out captions after cropping. Director can inspect and correct the same captions through `get_captions` and `set_captions`.

During a timeline edge drag, the viewer shows the actual incoming or outgoing source frame. The preview uses the same trim calculation as the final edit and works when the edge belongs to linked sound. **Freeze frame at playhead** splits the picture and its linked sound, inserts a silent hold, and moves the following clips on those tracks to make space.

Right-click empty timeline space to delete that gap or close every interior gap on a track. **Clip → Close All Gaps** uses the selected clips' tracks, or all unlocked tracks when nothing is selected. Linked clips on unlocked tracks move together and stop where another clip blocks them. An individual gap at the start of a track can be deleted. Closing every gap leaves the first clip's start in place on the track being compacted.

Choose **Match Sequence to Clip** from a source or timeline clip's context menu, or from **Clip** with a video selected. It uses the original video's display size and the closest supported frame rate. Undo restores the previous sequence format. Director can use the same gap and format commands.

A new project starts at 1920 × 1080 and 30 fps, matching Editroom on Mac. When you bring in the first video with a different format, choose **Match** or **Keep Current**. Images, audio, and imports by Director leave the choice available for the first video you import yourself. Manually setting the sequence size or rate confirms the settings. On an empty project with untouched settings, Quick Clips uses the render's size and frame rate.

Drag a single selected picture or title in the viewer to position it. The **Transform** inspector can edit or reset the scale and position of several selected pictures in one step. Scale is shown as a percentage and position in sequence pixels. Title sizes scale with the sequence height in the viewer, Director frame checks, and exports. **Zoom** opens the scale curve for a single picture. Choose In, In/Out, or Out, drag the time points, and choose eased or linear motion. The curve follows the clip when trimmed. Use the inspector's looks, three color wheels, and copy and paste controls to grade selected pictures. Custom .cube looks apply in the viewer, Director frame checks, and exports.

Drag the title or solid button from Media to place a five-second clip on the timeline. The preview snaps to the same edit points as a media drop. Dropping on **+ Video** creates a track and places the clip in one undoable edit. A locked target sends the clip to an unlocked picture track, or creates one if they are all locked.

The title inspector lists font families available on the studio host. `get_project` returns a title's full effective style, including its color and position. The viewer, export preview, and exported cut wrap titles at word boundaries and center each line using the host's title renderer. The previews show a browser-rendered title while the host preview loads.

The viewer offers **Fit**, **25%**, **50%**, **100%**, and **200%** magnification. At **100%**, each sequence pixel is shown at one display pixel. Scroll the picture when it exceeds the viewport. Press **F** to enter the full-screen viewer with transport, loop, timecode, and zoom controls, or **Esc** to return to the editor.

Captions in the viewer, export preview, and exported cut use the same host layout. The export preview places captions in the chosen output shape. Highlighted words change with speech timing while holding the same layout.

EditCastle converts HLG, PQ, Apple Log, and Apple Log 2 footage to Rec.709 before applying creative grades. The viewer, thumbnails, Director frame checks, and continuation frames use that conversion. Export uses the camera original and tags the converted cut as Rec.709. Earlier imports build a new playback copy when needed. Downloading the project includes the original source files.

The **Transitions** inspector controls both cuts beside a selected picture. Dissolve and Wipe overlap the pictures using available footage before or after the cut. Dip to Black fades each side at the cut. A shorter hatched band shows the portion the footage allows. The viewer, exported video, and Director frame checks share the same timing. Picture transitions keep linked audio in place.

Drag an audio clip's level line to adjust its gain. Double-click the line to return to 0 dB. Fades set in the inspector show the curve on the clip. Drag a fade point to change the timing while keeping the curve. **Remove Fades** clears the selected clips' fades in one edit.

Use **Marks** or **M** to create a marker, then click its flag to edit the name and color or jump to another marker. Drag the flag to move it. **I** and **O** set in and out at the playhead. Drag either bracket to change it. **Shift+X** clears both. Loop playback repeats the marked range when it is valid. The export dialog offers that range by default. Skimming previews a time beside the playhead and playback can start there. The ruler snaps to clip edges and marks, and timeline drags scroll when you reach the viewport edge. Zoom keeps the playhead in place using the toolbar, **=** and **−**, or a modified scroll gesture. **Shift+Z** fits the cut.

Drag the middle of the bottom scrollbar to pan in time or either end to zoom. Drag the side scrollbar to scroll tracks or pull either end to resize them. Focus either scrollbar to use the arrow keys, Page Up, Page Down, Home, End, and **+** or **−**. The ruler stays visible while the tracks scroll, and the timeline follows the playhead during playback as it reaches the edge. The range readout jumps to either marked point and shows the marked duration. **S**, **R**, **K**, and **Shift+S** toggle snapping, sticky edges, skimming, and audio scrubbing.

The timeline has per-track gain, mute, solo, and level meters and a stereo meter in the viewer. Drag a track's gain control to hear the change during playback, or click it to type a value. Right-click a track header to add or delete a track or choose a ducking source and preset. Deleting a track removes its clips and renumbers the remaining tracks, and one Undo restores the track and its clips. The arrow in an audio track header opens the ducking controls. Choose the track carrying dialogue and the reduction to apply to music when it plays. The browser and FFmpeg export use the same dialogue activity and attack and release timings. Select one or more audio clips and click **Even levels** to adjust them, or click an empty lane first to adjust all unlocked audio clips. Director can edit track levels, ducking, and track layout through MCP.

The clip inspector provides linear, exponential, logarithmic, and S-curve fades and speed-ramp controls. Select several clips to apply or clear their fades in one step. For mixed selections, **Video Fades** and **Audio Fades** apply separately. Reversed and retimed audio uses a private, lossless preview derived from the selected source range. Export uses that same audio timing, with clip and track gains, fades, and ducking applied afterwards. Reversed edits keep the expected source range when you trim or split them. Native project import carries over the fade curves and reversed source ranges.

Use **Playback** to set speed and audio volume for the selection. **Ramp** opens the speed graph with **In** and **In/Out** shapes. Drag its points or enter values to change the easing across the selected clips' footage. The timeline previews clip lengths, positions, and transitions as you drag. While paused, the playhead follows the selected ramp point and shows its source frame for video. Release to apply the ramp as one edit or press **Esc** to cancel. With **Sticky** enabled, touching clips and unlocked linked partners follow the changing edge. **Reset Playback** returns the selection to 100% speed, removes ramps, and resets audio volume in one undoable edit.

With **Strip** enabled, visible timeline clips show frames sampled along their source timing. The thumbnails follow trims, reverse playback, freezes, speed ramps, and clip grades. They reuse the studio's playback copies while you scroll and zoom.

Audio waveforms follow each clip's trimmed source range, playback direction, speed ramps, and clip gain. The outline updates its waveform as you drag a trim handle. Press **Esc** to cancel the trim. Copies of the same source share one waveform in the browser.

The Export dialog previews the whole timeline or its marked range with independent playback and scrubbing. Choose Source, a resolution preset, or a custom height. For square and portrait shapes, drag the crop window or move it with the arrow keys. The composition and captions preview inside the chosen crop. Pick MP4 or 48 kHz, 24-bit WAV, set a filename, and choose As mixed or a loudness target. The file goes to browser downloads or to a destination you select when the browser offers a save picker.

EditCastle remembers the last export filename with the project. Where the browser offers writable folder access, choose **Location → Choose a folder** to remember a destination on this device. EditCastle asks before replacing a file and checks again when the export is ready. If the folder is unavailable or the file changes, the completed export stays available in the studio's export queue.

When you choose a loudness target, EditCastle measures the mix over the export range and applies one gain adjustment to the output. The adjustment preserves the balance and dynamics of the mix. It stops raising the volume when the highest sample would reach −1 dBFS, so an export with strong peaks can finish below the target.

Exports run in the background one at a time. Each job uses the timeline and media versions saved when it entered the queue. Open the toolbar queue to see progress and estimated time, cancel waiting or running jobs, or download a finished file after reloading the editor. Completed exports in the queue also survive a studio restart. **Clear Finished** removes completed, failed, and cancelled entries and their server copies. A host restart interrupts waiting and running jobs. MCP clients can use `export_status` and `cancel_export` for the current project.

The local ledger starts with 180 credits. It reserves credits before submission and settles them when ComfyUI reports the outcome. Finished takes use GPU time even when they fail footage review. Local top-ups are for this self-hosted ledger. OpenAI and Anthropic bill their own API usage separately.

## Private GPU connection and Cloudflare Tunnel

Run ComfyUI on the GPU host's loopback address. Forward that port over SSH to the web server's host and set `EDITROOM_COMFY_URL` to the local forward, which defaults to `http://127.0.0.1:48188`. The Rust worker only accepts a loopback HTTP target and only calls the ComfyUI endpoints needed for Pi-H3. The web API serves its own project, editing, job progress, and media operations to authenticated studio sessions. Set `EDITROOM_COMFY_TUNNEL` to the absolute path of a host-side SSH launcher for automatic reconnection.

On workers with the per-job status API, Studio polls only the ComfyUI job ID it submitted. Older workers use the job's history and the shared queue over the same private connection. Each account's timeline, footage, and credit reservations stay in its own workspace.

In Director settings, **Check GPU** verifies that Studio can reach ComfyUI and find the selected video model's required node. The check uses the private connection and leaves render jobs and credits untouched.

On Vast, retire any template routes and quick tunnels for both ComfyUI and the ComfyUI API Wrapper. Keep the worker bound to loopback and use its default origin check. EditCastle retries downloads of already-rendered shots after interrupted transfers. A retry uses the same GPU job and credit reservation.

For remote use, configure the Supabase accounts described below and set `EDITROOM_PUBLIC_HOST=studio.example.com`. Configure your named Cloudflare Tunnel to route that exact HTTPS hostname to `http://127.0.0.1:4324`. Keep the tunnel connected to the application port. The public hostname uses the tunnel's `X-Forwarded-Proto` header to redirect HTTP page requests and accept changes over HTTPS. If you use another reverse proxy, configure it to replace that header with the visitor's actual scheme. The server rejects other hosts, sets Secure session cookies for the configured hostname, requires an exact same-origin header on writes, and requires studio sign-in for generation, projects, media, and export. Cloudflare Access can add an identity check in front of the studio. [Cloudflare header reference](https://developers.cloudflare.com/fundamentals/reference/http-headers/#x-forwarded-proto)

## Studio accounts

Set `EDITROOM_SUPABASE_URL` to the Supabase project URL and `EDITROOM_SUPABASE_PUBLISHABLE_KEY` to the project's publishable key. Enable the email provider and add the studio URL ending in `/studio` to Supabase's redirect allow list. New users sign up with an email and password, confirm their email, and return to sign in. EditCastle sends those credentials directly to Supabase and issues its own HTTP-only session cookie after a successful sign-in.

**Forgot password?** requests a reset email through Supabase. Keep the default confirmation link in the reset email template. After choosing a new password, the user signs in again and the studio closes that account's other browser sessions.

During an active session, EditCastle renews the Supabase session on the server when its access token expires. The browser keeps its studio cookie and Director keeps its connected planner. Sessions last up to twelve hours and can be ended with **Sign out**.

Account mode uses an independent project, media, export, and credit store under `studio-data/accounts/` for each confirmed Supabase account. New accounts start with zero render credits. Back up the account directories with the studio data. The standalone launch configuration still uses the studio access key for the existing local library.

For a local preview using Supabase Auth over loopback HTTP, set `EDITROOM_LOCAL_TEST_CREDITS=24` to start each new studio account with 24 credits. The starting balance is applied once when the account first opens the studio. Public deployments require the normal account credit flow.

If the local Auth service delivers email to a test inbox such as Mailpit, set `EDITROOM_LOCAL_TEST_INBOX_URL` to its loopback HTTP address. Sign-up and password reset link to that inbox when they send an email.

Set `EDITROOM_STRIPE_SECRET_KEY` to a test-mode Stripe key and `EDITROOM_STRIPE_WEBHOOK_SECRET` to its webhook signing secret. Point the test webhook at `/api/billing/stripe-webhook` and subscribe to `checkout.session.completed` and `checkout.session.async_payment_succeeded`. The test pack defaults to 100 credits for $5.00 USD. Set `EDITROOM_STRIPE_CREDITS` and `EDITROOM_STRIPE_PRICE_CENTS` to change the test pack. A signed, paid checkout adds the pack to the buying account once.

## Compare native timing

The Swift parity fixtures cover Pi-H3 shot timing, speed ramps, and snapping edits to the sequence frame grid. The studio stores edits on a 240,000-tick clock and uses the chosen sequence rate for editing, playback, and export. The video model keeps its 24 fps render grid. Existing local project data, undo history, and credit records are migrated automatically.

```sh
swiftc parity/native-timing.swift -o /absolute/scratch/editroom-native-timing
rust-anything app record /absolute/scratch --inputs /absolute/web/parity/timing-inputs.jsonl -- /absolute/scratch/editroom-native-timing
rust-anything app replay /absolute/scratch --build /absolute/web/target/debug/editcastle-web
```

Use `parity/native-ramp.swift` with `parity/ramp-inputs.jsonl`, or `parity/native-sequence.swift` with `parity/sequence-inputs.jsonl`, to check the other timing contracts in separate scratch directories. Use `parity/native-duck.swift` with `parity/duck-inputs.jsonl` to check the native ducking envelope, or `parity/native-fade.swift` with `parity/fade-inputs.jsonl` for the fade curves.

## Carry a native project over

In the editor, choose **File → Open Project** or drop a compatible project onto the workspace. Open a project ZIP, a `.room` document, or a legacy `.cutplay` document. For a standalone document, drop the referenced media and custom looks with it or select them in the dialog. Select a parent folder when several assets have the same filename. The browser uploads the matched files and opens an independent project.

**Open with Missing Media** keeps unavailable clips in place so you can keep editing. In Media, right-click a missing source and choose **Locate…** to select its file. **Locate missing media** checks a selected folder for matching files and leaves ambiguous matches for you to choose individually. Relinking keeps the source ID and clip positions. If a replacement is shorter, EditCastle trims clips to fit. While a source is missing, playback and export skip its picture and sound and keep its timeline duration.

In a browser that supports device folders, right-click an imported folder in Media and choose **Link original folder…** to resume syncing that folder. Files and subfolders excluded from the Mac project stay out of future syncs. Downloaded project ZIPs carry those exclusions.

Choose **File → Download Project** to save a ZIP with a native `.room` document, media, and custom looks. Unzip it before opening the document in Editroom on Mac. The browser can open the ZIP directly. Saved sequence and track settings, source trims, clip links, effects, markers, and the marked range carry across. Word timings and media notes stay with the browser copy. Relative custom look paths are supported by the updated Mac code.

The command-line importer is also available when the native project and its original media already live on the studio host. Stop the web server using the target data directory before running it:

```sh
target/debug/editcastle-web import-room "/path/to/Film.room"
```

The command-line importer reads associated media from the native project folder or original file locations and leaves the source `.room` file intact. When run on the Mac that transcribed the footage, it carries over cached word timings for matching files, including footage without captions.

## Connect an MCP client

A signed-in app session can call MCP at `/api/projects/{project-id}/mcp`. In **File → Editor Settings → Attached agents**, enable access to copy the open project's endpoint and a separate agent key. Paste the key into the MCP client's bearer credential setting. The setup text contains the endpoint and connection instructions.

The studio generates `mcp-key` in the workspace's data directory when first enabled. Standalone hosts can supply their own key through `EDITROOM_MCP_KEY_FILE`. Connect using Streamable HTTP. MCP clients that initialize a session should send the returned `Mcp-Session-Id` header with their subsequent requests. Use `DELETE` with that header to close the session. Turning attached agents off revokes those sessions. In-studio Director uses the signed-in browser session for editing and GPU generation.

For clients that launch a local stdio program, use **Copy stdio config** in Attached agents. Point the command at the `editcastle-web` binary on the client's machine and set `EDITROOM_MCP_KEY_FILE` to a file readable only by your user containing the agent key. Clients that manage environment secrets can set `EDITROOM_MCP_KEY` instead. The command starts with `editcastle-web mcp-stdio` followed by the project's copied MCP URL. Use HTTPS for a remote studio. The bridge carries the client name and session to the studio and disconnects it when the input closes.

To let a local stdio agent bring in its own footage, set `EDITROOM_MCP_MEDIA_DIR` in the bridge's environment to the absolute path of the media folder you want to share. The agent can call `import_media` with files or folders in that directory. The bridge copies supported media into the open project, preserves imported folder paths, and returns the new source IDs for editing. To keep a browser device folder in sync as files change, link it from the Media panel.

The stdio bridge reconnects after a Studio restart. It can safely resend a request when Studio rejects the expired session before dispatching a tool. If the connection drops after a request was sent, inspect the cut before sending the edit again. Direct HTTP clients can recognize the pre-dispatch rejection on a 404 response with the header `EditCastle-MCP-Session: expired`.

For Claude Desktop on Mac, select **Download Claude extension** in Attached agents and open the downloaded `.mcpb` file. The extension targets the open project. When Studio runs on the same Mac over loopback, it pre-fills the bridge executable and the private key file. For a remote Studio, select a local `editcastle-web` executable and a private file containing the Studio's agent key in Claude's extension settings. Each project has its own extension entry.

For Codex or Claude Code on the same computer, select the client in Attached agents and choose **Connect chosen client**. Studio uses the installed CLI to register the open project. **Remove chosen client** removes that project's entry. For a remote Studio, use **Copy install command** or **Copy remove command** and run it in Terminal on the client. Replace the executable and key-file placeholders in the install command first.

Attached agents shows whether the selected local client is registered for this project. It reads the client's settings when you open the panel, change the selected client, or complete a setup action. Active agent names appear separately when a client uses its connection.

When the project is open in the browser, timeline changes made by an attached agent appear under its named cursor. The preview shows the actual placement, move, or trim before the server saves it. Other clients can continue reading the saved cut during that animation. If a different edit wins the save, the preview clears and the agent gets an error. With no browser watching, the edit saves as soon as the tool finishes.

The MCP `playback` tool plays, pauses, or seeks the viewer for the open project. `get_project` and `get_frame` use the same playhead as the browser. Browser playback and the marked loop range are visible to the attached client. These controls leave Undo and Redo intact. During a Director draft, a seek sets the time for its next frame inspection.

`look_at_media` returns one timestamped contact sheet of a source video. It samples the middle of each interval by default. An agent can ask for up to twelve specific times or inspect a still at its original aspect ratio. `analyze_media` returns ranked source ranges, brightness, and detected faces for picture sources. For music it returns tempo, beats, and bar starts using the same analysis as Auto Edit.

`list_media` reports the source dimensions and the original video frame rate, including phone rotation. `add_clip` can hold a still for a specified duration. `get_project` links the still to its source asset. `get_transcript` reports whether it found speech, had no audio to read, or could not transcribe it. When `add_solid` has no target track, it uses the lowest free unlocked picture track. Titles use a free track above V1.

For a reversed clip, `get_project` reports its `source_start`, `source_end`, and `speed_ramp` on the playback-oriented source clock, as on Mac. `asset_source_start` and `asset_source_end` are the boundaries in the original file. Pass `coordinate_space: "asset"` to `set_speed_ramp` when using original timestamps from `look_at_media` or `analyze_media`. `trim_clip` accepts timeline positions or a pair of original-file `source_start` and `source_end` values.

Director uses the same tools in an isolated draft. The timeline shows each placement, move, trim, and marker update as it happens. Director pauses playback when it starts. While the timeline is locked, press **Space** to play or pause the current preview. Director can keep editing while a shot renders. It inspects the generated frames before accepting footage into the draft. Use **Stop** or **Escape** to return to the saved cut. When Director finishes, the full edit is saved as one Undo step. Quick Clips previews each accepted shot as it joins the cut.

Stopping during generation also asks ComfyUI to cancel that specific shot. Older workers can remove a queued shot, and a running shot on those workers may finish on the GPU. Credits settle against the worker's result. A finished render uses credits even when Director stopped before adding it to the cut.

The Activity panel updates while ChatGPT or Claude plans, searches, and prepares a tool call. It shows elapsed time during waits and keeps the duration when the run finishes. It also shows ChatGPT's short progress messages when the model supplies them. Draft edits wait for a complete tool call. If the provider stops early, Director saves the completed edits as one Undo step and marks the run **Partially saved**. At save time, Director merges independent changes made in another view while the draft was open. Competing edits to the same clip and placements that would overlap on the same track stop the save.

Quick Clips shows its shot plan in Activity as soon as it has one. New sequences append at the end of the current cut on the first unlocked picture track. Generated audio stays linked to its picture. A continuation starts at the end of the selected clip on its track and moves following clips and their linked audio to make room. If the selected clip changes during rendering, the reviewed shots stay in Media so you can place them yourself.

Refreshing the page shows the current run again. After an unexpected studio restart, shots already approved by Director or Quick Clips are available in Media and linked in the conversation. The timeline opens with the last saved cut. A cut committed before the restart is recorded as saved.

Use **Recover footage** in Director to retrieve a render that was interrupted before review. Connect a planner, then recover the original GPU job. The planner checks the footage and any starting frame before saving an approved take to Media. The approval also survives a restart during recovery. Recovery uses the original render and its credit charge. If the worker is still rendering, try again after it finishes.

If a run ends after some shots passed review, including when a competing edit prevents Director from saving its draft, choose **Append reviewed footage** on that run's conversation card. Studio adds the remaining approved shots, in order, after the existing cut as one undoable edit. The action uses the footage already in Media.

Type **@** to name a timeline clip, marker, library file, the selected clips, the playhead, or the marked range. Use **↑/↓** to browse, **Enter** or **Tab** to pick, and **Escape** to dismiss the list. Repeated clip and marker names are numbered in timeline order. Frame timecodes and ranges such as `from @marker:Intro to @marker:Outro` resolve against the project when you send the prompt. With the list closed, **Enter** sends, **Shift+Enter** adds a line, and **↑** in the empty box recalls the last prompt.

Select sources or folders in Media before asking Director to remove them from the project. Director previews the affected sources and clips, then shows the removal in its draft. Stopping the draft restores the saved media and cut. A finished removal and the other timeline changes can be undone together. If another edit changes media within a staged removal, Director asks you to review the updated project before trying again.

Ask Director to find music or sound for a cut. ChatGPT and Claude use their provider's web search. Director reads the track's public source page and checks for a licence that permits commercial use without attribution. It imports the audio to Media, places it on the draft timeline, and links to its source in the conversation. The track's licence and source URL are recorded in its Media notes. For Local, include a public source page or use files already in Media.

## Optional local models

On a macOS studio host, the media library indexes photos and video frames using Apple Vision. Enter plain words to search names and visual labels. Use quotes to find a spoken phrase. Open a result at its matching source time. Director can use the same results through MCP. Install `whisper-cli` and set `EDITROOM_WHISPER_MODEL` to a Whisper base GGML model path to enable automatic local speech indexing. The index is stored alongside the private media on the host.

Select **Local** in Director to edit with the studio host and its Media. It works without an API key. Try “Build a five-second square montage using every clip”, “Make the selected clip black and white at double speed”, or “Add the title ‘City Walk’ at the end”. Local accepts clip mentions, the playhead and marked ranges, timing, pacing, looks, titles, music, audio levelling, speed ramps, and sound effects from Media. Captions use the host transcription setup above. The draft shows the changes and saves the request as one Undo step.

For a tool-capable local inference server, set `EDITROOM_LOCAL_URL` to its loopback HTTP address and `EDITROOM_LOCAL_MODEL` to the model name. Local then uses that server for planning. Footage review requires vision support.

Browser and GPU integration tests use a separate project and a deterministic planner fixture. Live API provider sessions require a real OpenAI or Anthropic key.

## Rebuild the landing reel

The landing page plays a muted Remotion composition using a capture of the browser editor and the three films from the gallery. The rendered video and its poster ship with the Rust app. To update them, run `npm ci` and `npm run render` from `web/reel`.
