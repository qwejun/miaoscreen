package com.bongocat.catscreen;

import android.app.Activity;
import android.graphics.Color;
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
 */
public final class MainActivity extends Activity {

    private static final String TAG = "CatScreen";

    private GLSurfaceView surface;
    private TextView status;
    private PublisherClient client;
    private Thread clientThread;

    @Override
    protected void onCreate(Bundle savedState) {
        super.onCreate(savedState);

        getWindow().addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON);
        getWindow().setFlags(
                WindowManager.LayoutParams.FLAG_FULLSCREEN,
                WindowManager.LayoutParams.FLAG_FULLSCREEN);

        FrameLayout root = new FrameLayout(this);
        root.setBackgroundColor(Color.BLACK);

        client = new PublisherClient();

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
        status.setTextSize(TypedValue.COMPLEX_UNIT_SP, 16f);
        status.setGravity(Gravity.CENTER);
        status.setText("CatScreen\nwaiting for the publisher\n"
                + Build.MANUFACTURER + " " + Build.MODEL
                + "  ·  Android " + Build.VERSION.RELEASE);
        root.addView(status, new FrameLayout.LayoutParams(-1, -2, Gravity.TOP));

        renderer.setStats(new CatRenderer.Stats() {            @Override
            public void onStats(final String text, final boolean live) {
                runOnUiThread(new Runnable() {
                    @Override
                    public void run() {
                        status.setVisibility(live ? View.GONE : View.VISIBLE);
                        if (!live) {
                            status.setText(text);
                        }
                    }
                });
            }
        });

        client.setListener(new PublisherClient.Listener() {
            @Override
            public void onStatus(final String text, final boolean live) {
                runOnUiThread(new Runnable() {
                    @Override
                    public void run() {
                        status.setVisibility(live ? View.GONE : View.VISIBLE);
                        if (!live) {
                            status.setText("CatScreen\n" + text);
                        }
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
        hideSystemUi();
    }

    @Override
    protected void onPause() {
        surface.onPause();
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
