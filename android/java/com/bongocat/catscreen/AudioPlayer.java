package com.bongocat.catscreen;

import android.content.Context;
import android.media.AudioAttributes;
import android.media.MediaPlayer;
import android.util.Log;

import java.io.File;
import java.io.FileOutputStream;
import java.io.IOException;
import java.util.HashMap;
import java.util.Map;

/**
 * Plays the model's sounds on the panel's own speaker.
 *
 * <p>The desktop sends the clips once with the textures and then only an index
 * per keystroke, so a noise costs thirteen bytes on the wire instead of a file.
 * Clips are written to the cache directory and handed to {@link MediaPlayer} by
 * path, which lets the platform decode whatever the model shipped — WAV, OGG,
 * FLAC, MP3 — without this class knowing which.
 *
 * <p>One player exists per clip and is kept between plays. A {@code MediaPlayer}
 * costs tens of milliseconds to prepare, and a keystroke sound that starts after
 * the keystroke is worse than no sound at all.
 */
final class AudioPlayer {

    private static final String TAG = "CatScreen";

    private final Context context;
    private final Map<Integer, MediaPlayer> players = new HashMap<>();
    private int playing = -1;

    AudioPlayer(Context context) {
        this.context = context;
    }

    /** Replaces the library. A clip that will not prepare is reported once. */
    void setLibrary(byte[] payload, int offset, int count) {
        stop();
        releaseAll();
        File directory = new File(context.getCacheDir(), "clips");
        if (!directory.isDirectory() && !directory.mkdirs()) {
            Log.w(TAG, "cannot create the clip directory");
            return;
        }
        for (int i = 0; i < count; i++) {
            int id = readLe32(payload, offset);
            offset += 4;
            int size = readLe32(payload, offset);
            offset += 4;
            if (id < 0 || size < 0 || offset + size > payload.length) {
                Log.w(TAG, "clip " + id + " is truncated");
                return;
            }
            File file = new File(directory, "clip-" + id);
            if (!write(file, payload, offset, size)) {
                offset += size;
                continue;
            }
            offset += size;
            MediaPlayer player = prepare(file);
            if (player != null) {
                players.put(id, player);
            }
        }
        Log.i(TAG, "audio library: " + players.size() + " clips");
    }

    /** Starts one clip from the beginning, replacing whatever is playing. */
    void play(int id, float volume) {
        MediaPlayer player = players.get(id);
        if (player == null) {
            return;
        }
        stop();
        try {
            player.setVolume(volume, volume);
            player.seekTo(0);
            player.start();
            playing = id;
        } catch (IllegalStateException error) {
            Log.w(TAG, "clip " + id + " would not start: " + error.getMessage());
            players.remove(id);
            player.release();
        }
    }

    void stop() {
        MediaPlayer player = playing < 0 ? null : players.get(playing);
        playing = -1;
        if (player == null) {
            return;
        }
        try {
            if (player.isPlaying()) {
                player.pause();
            }
        } catch (IllegalStateException ignored) {
            // A player that already finished is not an error to report.
        }
    }

    void releaseAll() {
        for (MediaPlayer player : players.values()) {
            try {
                player.release();
            } catch (RuntimeException ignored) {
                // Releasing twice is harmless; the map is dropped either way.
            }
        }
        players.clear();
        playing = -1;
    }

    private MediaPlayer prepare(File file) {
        MediaPlayer player = new MediaPlayer();
        player.setAudioAttributes(new AudioAttributes.Builder()
                .setUsage(AudioAttributes.USAGE_MEDIA)
                .setContentType(AudioAttributes.CONTENT_TYPE_SONIFICATION)
                .build());
        try {
            player.setDataSource(file.getAbsolutePath());
            player.prepare();
            return player;
        } catch (IOException | IllegalStateException | IllegalArgumentException error) {
            Log.w(TAG, "clip " + file.getName() + " did not load: " + error.getMessage());
            player.release();
            return null;
        }
    }

    private static boolean write(File file, byte[] source, int offset, int size) {
        try (FileOutputStream out = new FileOutputStream(file)) {
            out.write(source, offset, size);
            return true;
        } catch (IOException error) {
            Log.w(TAG, "could not write " + file.getName() + ": " + error.getMessage());
            return false;
        }
    }

    private static int readLe32(byte[] source, int offset) {
        return (source[offset] & 0xFF)
                | ((source[offset + 1] & 0xFF) << 8)
                | ((source[offset + 2] & 0xFF) << 16)
                | ((source[offset + 3] & 0xFF) << 24);
    }
}
