# EditCastle

EditCastle is a browser-based video editor and AI video studio. Its Rust server hosts the editor, handles project media and exports, and connects Director to a private ComfyUI worker for video generation. Director plans shots from a prompt, uses the previous shot's final frame to start the next one, and reviews the footage before adding it to the timeline.

## Run locally

Install Rust, FFmpeg, and FFprobe before running the launcher.

```sh
cd web
./start-editcastle.sh
```

Open the [local landing page](http://127.0.0.1:4324) or the [studio](http://127.0.0.1:4324/studio). The [setup guide](web/README.md) covers Supabase sign-in, Stripe test credits, the private GPU connection, and MCP clients.

## Credits

EditCastle builds on [Editroom](https://github.com/robbietilton/Editroom) by Wonder Assembly LLC. The original copyright and MIT terms are included in [LICENSE](LICENSE).
