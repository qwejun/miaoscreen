package com.bongocat.catscreen;

import android.app.Activity;
import android.content.Context;
import android.content.Intent;
import android.graphics.Color;
import android.net.wifi.WifiManager;
import android.opengl.GLSurfaceView;
import android.os.Build;
import android.os.Bundle;
import android.util.Log;
import android.util.TypedValue;
import android.view.Gravity;
import android.view.View;
import android.view.WindowManager;
import android.widget.FrameLayout;
import android.widget.TextView;

/**
 * The round-screen client.
 *
 * <p>The activity owns a fullscreen GL surface and a status line that is only
 * visible while there is nothing to draw: a live picture needs no chrome, and the
 * panel has no bezel to hide an overlay in.
 *
 * <p>The status line says one word. Frame rate, resolution, mask and key counters
 * belong in logcat, where they are readable without covering the picture — a
 * panel this size has room for a cat or for diagnostics, not both.
 */
public final class MainActivity extends Activity {

    private static final String TAG = "CatScreen";

    private GLSurfaceView surface;
    private TextView status;
    private PublisherClient client;
    private Thread clientThread;
    /**
     * Keeps the radio out of power save.
     *
     * <p>A station that sleeps is served on a beacon, so the access point holds
     * every frame until the next delivery window. Nothing is lost, every counter
     * on the link says it is healthy, and the picture is a third of a second
     * behind. This lock is the only thing on this side that turns that off.
     */
    private WifiManager.WifiLock wifiLock;

    @Override
    protected void onCreate(Bundle savedState) {
        super.onCreate(savedState);

        // The panel is looked at, not operated: a display that dims after a few
        // idle seconds makes the cat disappear at exactly the moment nobody is
        // touching anything. Three things are needed, and none of them is
        // sufficient alone — keep-screen-on stops the display from sleeping, the
        // window brightness overrides the automatic curve that dims it first, and
        // the turn-screen-on flags make a cold start wake a unit that was already
        // dark instead of painting into a sleeping panel.
        getWindow().addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON);
        getWindow().addFlags(WindowManager.LayoutParams.FLAG_TURN_SCREEN_ON);
        getWindow().addFlags(WindowManager.LayoutParams.FLAG_DISMISS_KEYGUARD);
        getWindow().setFlags(
                WindowManager.LayoutParams.FLAG_FULLSCREEN,
                WindowManager.LayoutParams.FLAG_FULLSCREEN);
        applyBrightness();

        FrameLayout root = new FrameLayout(this);
        root.setBackgroundColor(Color.BLACK);
        root.setKeepScreenOn(true);

        // `--es address host:port` points the client at a publisher reached over
        // the LAN; without it the loopback default is adb's reverse tunnel.
        WifiManager wifi = (WifiManager) getApplicationContext()
                .getSystemService(Context.WIFI_SERVICE);
        if (wifi != null) {
            wifiLock = wifi.createWifiLock(WifiManager.WIFI_MODE_FULL_HIGH_PERF, "CatScreen:frame");
            wifiLock.setReferenceCounted(false);
        }

        client = new PublisherClient(this, getIntent().getStringExtra("address"));

        surface = new GLSurfaceView(this);
        surface.setEGLContextClientVersion(2);
        surface.setEGLConfigChooser(8, 8, 8, 8, 16, 0);
        // The model's atlases are uploaded once, at the head of a connection, and
        // the host does not resend them on request: a context destroyed by a trip
        // to the launcher costs a full reconnect and a screen with no cat until it
        // finishes. Keeping the context across a pause makes that trip free, and
        // the renderer still knows what to do on the occasions the driver takes it
        // away anyway.
        surface.setPreserveEGLContextOnPause(true);
        CatRenderer renderer = new CatRenderer(client);
        // Parsed here rather than on the GL thread: each face is up to 3MB and
        // `createFromAsset` reads the whole file, which is not time the renderer
        // has to spare. A missing face is not fatal — that slot falls back to
        // the panel's sans-serif.
        renderer.setBundledTypefaces(CatRenderer.loadBundledTypefaces(getAssets()));
        surface.setRenderer(renderer);
        surface.setRenderMode(GLSurfaceView.RENDERMODE_CONTINUOUSLY);
        renderer.setDebugSwitches(
                getIntent().getBooleanExtra("skip_masks", false),
                getIntent().getBooleanExtra("skip_model", false));
        renderer.setForcedKey(getIntent().getIntExtra("fake_key", -1));
        root.addView(surface, new FrameLayout.LayoutParams(-1, -1));

        status = new TextView(this);
        status.setTextColor(Color.WHITE);
        status.setShadowLayer(8f, 0f, 0f, Color.BLACK);
        status.setTextSize(TypedValue.COMPLEX_UNIT_SP, 18f);
        status.setGravity(Gravity.CENTER);
        status.setText("正在连接…");
        root.addView(status, new FrameLayout.LayoutParams(-1, -1, Gravity.CENTER));

        renderer.setStats(new CatRenderer.Stats() {
            @Override
            public void onStats(final String text, final boolean live) {
                // The counters themselves go to logcat inside the renderer; the
                // panel only needs to know whether there is a picture, and a live
                // picture needs no words on it at all.
                if (!live) {
                    return;
                }
                runOnUiThread(new Runnable() {
                    @Override
                    public void run() {
                        showStatus(null);
                    }
                });
            }
        });

        client.setListener(new PublisherClient.Listener() {
            @Override
            public void onStatus(final String text, final boolean live) {
                Log.i(TAG, "link: " + text);
                final String label = live ? null : labelFor(text);
                runOnUiThread(new Runnable() {
                    @Override
                    public void run() {
                        showStatus(label);
                    }
                });
            }
        });

        setContentView(root);

        clientThread = new Thread(client, "publisher-client");
        clientThread.start();

        Log.i(TAG, "manufacturer=" + Build.MANUFACTURER + " model=" + Build.MODEL
                + " sdk=" + Build.VERSION.SDK_INT + " release=" + Build.VERSION.RELEASE);
    }

    /**
     * A second launch, which is how the host moves the panel onto another path.
     *
     * <p>The activity is `singleTask`, so `adb shell am start` does not build a
     * second surface: it arrives here. The panel stays connected across the
     * change, which is the whole point — restarting the app would drop the
     * window and flash the screen the user is looking at.
     */
    @Override
    protected void onNewIntent(Intent intent) {
        super.onNewIntent(intent);
        setIntent(intent);
        String address = intent != null ? intent.getStringExtra("address") : null;
        if (client != null && address != null) {
            client.switchAddress(address);
        }
    }

    @Override
    protected void onResume() {
        super.onResume();
        surface.onResume();
        // Re-applied on every resume: the brightness is a window attribute, and a
        // trip through the background gives the policy manager a chance to hand
        // the display back to the system curve.
        applyBrightness();
        if (wifiLock != null && !wifiLock.isHeld()) {
            wifiLock.acquire();
            Log.i(TAG, "wifi high performance lock held");
        }
        hideSystemUi();
    }

    @Override
    protected void onPause() {
        surface.onPause();
        if (wifiLock != null && wifiLock.isHeld()) {
            wifiLock.release();
        }
        super.onPause();
    }

    @Override
    protected void onDestroy() {
        if (client != null) {
            client.stop();
        }
        if (clientThread != null) {
            clientThread.interrupt();
            clientThread = null;
        }
        super.onDestroy();
    }

    @Override
    public void onWindowFocusChanged(boolean hasFocus) {
        super.onWindowFocusChanged(hasFocus);
        if (hasFocus) {
            hideSystemUi();
        }
    }

    /**
     * Turns the client's internal status into the one line the panel shows.
     *
     * <p>Three states, and they are the three a person standing in front of this
     * thing can act on: waiting for a host, connected but nothing to draw yet, or
     * the link dropped and it is trying again. Anything more specific than that
     * is a developer's question, and logcat is where it is answered.
     */
    private static String labelFor(String text) {
        if (text.startsWith("live")) {
            return null;
        }
        if (text.startsWith("connected")) {
            return "已连接，等待画面…";
        }
        if (text.startsWith("loading the model")) {
            return "正在加载模型…";
        }
        return "连接中断，重试中…";
    }

    /**
     * Shows the panel's only chrome: one line while there is nothing to draw.
     *
     * <p>A live picture needs no words on top of it — this panel has room for a
     * cat or for text, not both.
     */
    private void showStatus(String label) {
        if (label == null) {
            status.setVisibility(View.GONE);
            return;
        }
        status.setText(label);
        status.setVisibility(View.VISIBLE);
    }

    /**
     * Pins this window's brightness at the top of the range.
     *
     * <p>A window brightness replaces the system curve rather than scaling it, so
     * this holds the panel at full output and, more importantly, removes the step
     * where the policy manager dims the display before sleeping it. The attribute
     * is a float in 0..1, with -1 meaning "follow the system" — the default this
     * replaces. It belongs to the window, so it lasts exactly as long as this
     * activity is in front.
     */
    private void applyBrightness() {
        WindowManager.LayoutParams attributes = getWindow().getAttributes();
        if (attributes.screenBrightness == 1.0f) {
            return;
        }
        attributes.screenBrightness = 1.0f;
        getWindow().setAttributes(attributes);
        Log.i(TAG, "window brightness pinned at 1.0");
    }

    private void hideSystemUi() {
        getWindow().getDecorView().setSystemUiVisibility(
                View.SYSTEM_UI_FLAG_LAYOUT_STABLE
                        | View.SYSTEM_UI_FLAG_LAYOUT_HIDE_NAVIGATION
                        | View.SYSTEM_UI_FLAG_LAYOUT_FULLSCREEN
                        | View.SYSTEM_UI_FLAG_HIDE_NAVIGATION
                        | View.SYSTEM_UI_FLAG_FULLSCREEN
                        | View.SYSTEM_UI_FLAG_IMMERSIVE_STICKY);
    }
}
