package com.bongocat.catscreen;

import android.content.Context;
import android.media.AudioAttributes;
import android.media.AudioManager;
import android.media.MediaPlayer;
import android.util.Log;

import java.io.File;
import java.io.FileOutputStream;
import java.io.IOException;
import java.util.HashMap;
import java.util.HashSet;
import java.util.Map;
import java.util.Set;
import java.util.concurrent.ExecutorService;
import java.util.concurrent.Executors;

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
 *
 * <p>Loading runs on its own thread. A library is a few megabytes of writes plus
 * one prepare per clip, and doing that on the socket thread stops the reads; the
 * host's writes then back up until its own two-second write timeout closes a
 * connection that was never broken.
 */
final class AudioPlayer {

    private static final String TAG = "CatScreen";

    private final Context context;
    private final AudioManager audio;
    private final Map<Integer, MediaPlayer> players = new HashMap<>();
    private int playing = -1;
    /** Ids already reported as unloaded, so a missing clip logs once. */
    private final Set<Integer> missing = new HashSet<>();
    /** Set once the media stream has been aligned, after a clip actually played. */
    private boolean volumeAligned;

    /** One library load at a time, ordered, off the socket thread. */
    private final ExecutorService loader = Executors.newSingleThreadExecutor(
            new java.util.concurrent.ThreadFactory() {
                @Override
                public Thread newThread(Runnable runnable) {
                    Thread thread = new Thread(runnable, "audio-library");
                    thread.setDaemon(true);
                    return thread;
                }
            });

    AudioPlayer(Context context) {
        this.context = context;
        this.audio = (AudioManager) context.getSystemService(Context.AUDIO_SERVICE);
    }

    /**
     * Raises the media stream the first time a clip plays.
     *
     * <p>Volume in this framework is indexed per output device, and this unit
     * boots with {@code speaker: 1} while the default device sits at 8. The keys
     * on the panel only move the device that is active at the time, so pressing
     * volume up while nothing is playing adjusts a route the sounds never take.
     * Calling this *after* {@code start()} is what makes the adjustment land on
     * the speaker: by then the stream has been routed there.
     */
    private void alignVolume() {
        if (audio == null || volumeAligned) {
            return;
        }
        volumeAligned = true;
        int maximum = audio.getStreamMaxVolume(AudioManager.STREAM_MUSIC);
        if (maximum <= 0) {
            return;
        }
        int current = audio.getStreamVolume(AudioManager.STREAM_MUSIC);
        int target = Math.max(1, (maximum * 3) / 5);
        Log.i(TAG, "media volume " + current + " of " + maximum + " while playing");
        if (current < target) {
            audio.setStreamVolume(AudioManager.STREAM_MUSIC, target, 0);
            Log.i(TAG, "media stream raised " + current + " -> " + target);
        }
    }

    /**
     * Hands a new library to the loader and returns immediately.
     *
     * <p>{@code payload} is already a private copy, so the socket thread can
     * carry on reading the moment this returns.
     */
    void setLibrary(final byte[] payload, final int offset, final int count) {
        loader.execute(new Runnable() {
            @Override
            public void run() {
                load(payload, offset, count);
            }
        });
    }

    /** Replaces the library. A clip that will not prepare is reported once. */
    private synchronized void load(byte[] payload, int offset, int count) {
        stop();
        releaseAll();
        File directory = new File(context.getCacheDir(), "clips");
        if (!directory.isDirectory() && !directory.mkdirs()) {
            Log.w(TAG, "cannot create the clip directory");
            return;
        }
        for (int i = 0; i < count; i++) {
            // The header of one entry has to fit before it can be read: a frame
            // cut short by a dropped connection stops here instead of throwing.
            if (offset < 0 || offset + 8 > payload.length) {
                Log.w(TAG, "audio library ends after " + i + " of " + count + " clips");
                return;
            }
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
        missing.clear();
        Log.i(TAG, "audio library: " + players.size() + " clips");
    }

    /** Starts one clip from the beginning, replacing whatever is playing. */
    synchronized void play(int id, float volume) {
        MediaPlayer player = players.get(id);
        if (player == null) {
            // Once per id: a model whose sounds never arrived would otherwise fill
            // the log at keystroke rate, and the first line already says it all.
            if (missing.add(id)) {
                Log.w(TAG, "clip " + id + " is not loaded (" + players.size() + " ready)");
            }
            return;
        }
        stop();
        try {
            player.setVolume(volume, volume);
            player.seekTo(0);
            player.start();
            playing = id;
            alignVolume();
        } catch (IllegalStateException error) {
            Log.w(TAG, "clip " + id + " would not start: " + error.getMessage());
            players.remove(id);
            player.release();
        }
    }

    synchronized void stop() {
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

    synchronized void releaseAll() {
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

    /** Stops the loader and frees every player. Used when the client shuts down. */
    void close() {
        loader.shutdownNow();
        releaseAll();
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
