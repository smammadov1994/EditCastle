# Render shots in EditCastle

A duration for the video is the runtime of the complete cut. A ten-second video with no requested cuts can be one ten-second shot. When asked for ten seconds across two shots, split the ten seconds between the shots. Each clip and per shot refer to individual shot lengths. Two ten-second clips make a twenty-second edit. Preserve the established shot count and direction when changing the length of an existing sequence unless the editor requests another change.

When the editor sets Seconds per shot, use that duration for each shot. If they also specify a total runtime, choose a shot count that fits both or ask which length to change.

When the editor specifies neither a runtime nor a shot count, use three shots of about five seconds for a sequence. Use one for an isolated action. Each shot can run from one to fifteen seconds. If a continuous shot needs longer, split it into segments connected by matching frames. Ask before rendering if the requested total, shot count, and shot durations conflict.

1. Read the project and media library. Use one main action per shot and describe the start and end poses, camera framing, screen direction, and relevant audio.
2. Call `render_video_shot` using exact seconds for the intended timeline duration. With a continuation, supply the previous placed video clip ID in `continue_from_clip_id` to use its final visible frame after trims. Keep its pose, subject, and screen direction consistent. The tool returns a shot ID while the render runs. One shot can render at a time, and you can make other edits while you wait.
3. Inspect the actual frames using `review_video_shot`. If its status is `rendering`, call it again with the same shot ID. Once the frames arrive, check whether the motion reads in order and whether the subjects, camera, and setting carry across the cut. Look for accidental reversals, repeats, jumps, and malformed footage. Request more samples where an action is unclear.
4. In a later reply, after receiving and inspecting the frames, record a decision with `decide_video_shot`. Give a concrete visual reason for acceptance or rejection. Retry only the rejected beat. Use the accepted asset ID in `add_clip` and its video clip ID to continue the next beat. Keep the preceding ending frame or regenerate later shots if you change it.
5. Check the edited composite with `get_frame` before finishing. If a beat could not be made to work, tell the editor which beat and preserve unrelated edits.

Pi-H3 and HyperFlow run privately through EditCastle. A ten-second clip is 240 visible frames and 243 worker frames. Allow EditCastle to choose and trim the valid worker length.
