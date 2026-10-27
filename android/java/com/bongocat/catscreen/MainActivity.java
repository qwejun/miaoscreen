package com.bongocat.catscreen;

import android.app.Activity;
import android.content.Context;
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
        CatRenderer renderer = new CatRenderer(client);
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
        status.setText("loading");
        root.addView(status, new FrameLayout.LayoutParams(-1, -1, Gravity.CENTER));

        renderer.setStats(new CatRenderer.Stats() {
            @Override
            public void onStats(final String text, final boolean live) {
                // The counters themselves go to logcat inside the renderer; all
                // the panel shows is whether there is a picture to look at.
                runOnUiThread(new Runnable() {
                    @Override
                    public void run() {
                        showStatus(live);
                    }
                });
            }
        });

        client.setListener(new PublisherClient.Listener() {
            @Override
            public void onStatus(final String text, final boolean live) {
                Log.i(TAG, "link: " + text);
                runOnUiThread(new Runnable() {
                    @Override
                    public void run() {
                        showStatus(live);
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
     * Shows the panel's only chrome: a single word while there is nothing to
     * draw. Anything more — frame age, frame rate, which half of the link failed —
     * is in logcat, because on this panel it would sit on top of the cat.
     */
    private void showStatus(boolean live) {
        status.setVisibility(live ? View.GONE : View.VISIBLE);
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
