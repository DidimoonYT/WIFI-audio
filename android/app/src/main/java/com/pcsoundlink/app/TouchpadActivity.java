package com.pcsoundlink.app;

import android.graphics.Bitmap;
import android.os.Build;
import android.os.Bundle;
import android.os.Handler;
import android.os.Looper;
import android.text.Editable;
import android.text.TextWatcher;
import android.view.MotionEvent;
import android.view.View;
import android.view.WindowManager;
import android.view.inputmethod.InputMethodManager;
import android.widget.EditText;
import android.widget.ImageView;
import android.widget.TextView;
import android.widget.Toast;

import androidx.appcompat.app.AppCompatActivity;
import androidx.core.graphics.Insets;
import androidx.core.view.ViewCompat;
import androidx.core.view.WindowCompat;
import androidx.core.view.WindowInsetsCompat;
import androidx.core.view.WindowInsetsControllerCompat;

import com.google.android.material.button.MaterialButton;

/**
 * Весь экран телефона — тачпад, а картинка экрана ПК нарисована поверх.
 * Жесты:
 *  • один палец      — курсор;
 *  • тап             — левый клик;
 *  • долгое нажатие  — правый клик;
 *  • два пальца      — прокрутка (колёсико);
 *  • тап двумя       — правый клик;
 *  • кнопка «Клавиатура» — ввод текста на ПК;
 *  • телефон         — поворот экрана работает автоматически.
 */
public class TouchpadActivity extends AppCompatActivity implements TouchpadClient.Listener {

    private static final int LONG_PRESS_MS = 450;
    private static final int MAX_TAP_MS = 250;
    private static final int MAX_TAP_DIST = 24;
    private static final int DOUBLE_TAP_MS = 350;
    private static final int DOUBLE_TAP_DIST = 90;
    private static final float MOVE_SENSITIVITY = 2.0f;
    private static final float SCROLL_SENSITIVITY = 4.0f;

    private TouchpadClient client;
    private ImageView screenView;
    private MaterialButton btnToggle;
    private TextView statusView;
    private EditText keyboardInput;
    private Handler handler;

    // состояние жестов
    private float lastX, lastY, lastAvgY;
    private float moveDistanceF;
    private int pointerCount;
    private long downAtMs;
    private boolean moved;
    private boolean longPressFired;
    private boolean scrolling;
    private boolean multiGestureUsed;
    private boolean rightClickPending;

    // двойное касание зажимает левую кнопку мыши (перетаскивание окон)
    private boolean leftButtonHeld;
    private long lastTapAtMs;
    private float lastTapX, lastTapY;
    private Runnable pendingSingleClick;

    private final Runnable longPressRunnable = () -> {
        if (!moved && pointerCount == 1 && !leftButtonHeld) {
            longPressFired = true;
            cancelPendingClick();
            client.sendMouse("click", "right", false, 0, 0);
        }
    };

    // состояние клавиатуры
    private String lastText = "";
    private boolean suppressText = false;

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        handler = new Handler(Looper.getMainLooper());
        setContentView(R.layout.activity_touchpad);

        String host = getIntent().getStringExtra("host");
        if (host == null || host.isEmpty()) {
            finish();
            return;
        }

        screenView = findViewById(R.id.screenView);
        statusView = findViewById(R.id.touchpadStatus);
        MaterialButton btnClose = findViewById(R.id.btnTouchpadClose);
        btnToggle = findViewById(R.id.btnTogglePreview);
        MaterialButton btnKeyboard = findViewById(R.id.btnKeyboard);
        keyboardInput = findViewById(R.id.keyboardInput);
        View touchpad = findViewById(R.id.touchpadArea);

        btnClose.setOnClickListener(v -> finish());
        btnToggle.setOnClickListener(v -> {
            boolean visible = screenView.getVisibility() == View.VISIBLE;
            screenView.setVisibility(visible ? View.GONE : View.VISIBLE);
            btnToggle.setText(visible ? R.string.touchpad_show_preview
                    : R.string.touchpad_hide_preview);
        });
        btnKeyboard.setOnClickListener(v -> openKeyboard());

        client = new TouchpadClient(getApplicationContext(), host);
        client.setListener(this);
        client.start();

        touchpad.setOnTouchListener(this::onTouchpadTouch);
        setupKeyboardInput();
        setupCutoutPadding();
        applyImmersive();
    }

    @Override
    public void onWindowFocusChanged(boolean hasFocus) {
        super.onWindowFocusChanged(hasFocus);
        if (hasFocus) {
            applyImmersive();
        }
    }

    private void applyImmersive() {
        getWindow().addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON);
        WindowInsetsControllerCompat controller = WindowCompat
                .getInsetsController(getWindow(), getWindow().getDecorView());
        controller.hide(WindowInsetsCompat.Type.systemBars());
        controller.setSystemBarsBehavior(
                WindowInsetsControllerCompat.BEHAVIOR_SHOW_TRANSIENT_BARS_BY_SWIPE);
    }

    /** Разрешаем рисовать под «чёлкой», а верхнюю панель опускаем ниже неё. */
    private void setupCutoutPadding() {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.P) {
            WindowManager.LayoutParams lp = getWindow().getAttributes();
            lp.layoutInDisplayCutoutMode =
                    WindowManager.LayoutParams.LAYOUT_IN_DISPLAY_CUTOUT_MODE_SHORT_EDGES;
            getWindow().setAttributes(lp);
        }
        View root = findViewById(R.id.touchpadRoot);
        final View topBar = findViewById(R.id.touchpadTopBar);
        final int baseTop = topBar.getPaddingTop();
        ViewCompat.setOnApplyWindowInsetsListener(root, (v, insets) -> {
            Insets cut = insets.getInsets(WindowInsetsCompat.Type.displayCutout());
            topBar.setPadding(topBar.getPaddingLeft(), baseTop + cut.top,
                    topBar.getPaddingRight(), topBar.getPaddingBottom());
            return insets;
        });
    }

    // ------------------------------------------------------------ screen stream

    @Override
    public void onStatus(String message) {
        statusView.setText(message);
    }

    @Override
    public void onFrame(Bitmap frame) {
        screenView.setImageBitmap(frame);
    }

    // ---------------------------------------------------------------- keyboard

    private void openKeyboard() {
        keyboardInput.requestFocus();
        keyboardInput.setSelection(keyboardInput.getText().length());
        InputMethodManager imm = (InputMethodManager)
                getSystemService(INPUT_METHOD_SERVICE);
        if (imm != null) {
            imm.showSoftInput(keyboardInput, InputMethodManager.SHOW_IMPLICIT);
        }
    }

    private void setupKeyboardInput() {
        keyboardInput.addTextChangedListener(new TextWatcher() {
            @Override
            public void beforeTextChanged(CharSequence s, int start, int count, int after) {
            }

            @Override
            public void onTextChanged(CharSequence s, int start, int before, int count) {
            }

            @Override
            public void afterTextChanged(Editable s) {
                if (suppressText) {
                    return;
                }
                String now = s.toString();
                String old = lastText;
                int p = 0;
                int min = Math.min(old.length(), now.length());
                while (p < min && old.charAt(p) == now.charAt(p)) {
                    p++;
                }
                int removed = old.length() - p;
                String added = now.substring(p);
                lastText = now;
                if (removed == 0 && added.isEmpty()) {
                    return;
                }
                StringBuilder out = new StringBuilder();
                for (int i = 0; i < removed; i++) {
                    out.append('\b');
                }
                out.append(added);
                client.sendText(out.toString());
            }
        });
    }

    // ---------------------------------------------------------------- touchpad

    private boolean onTouchpadTouch(View v, MotionEvent event) {
        switch (event.getActionMasked()) {
            case MotionEvent.ACTION_DOWN:
                cancelPendingClick();
                pointerCount = 1;
                moved = false;
                moveDistanceF = 0;
                scrolling = false;
                multiGestureUsed = false;
                rightClickPending = false;
                longPressFired = false;
                lastX = event.getX();
                lastY = event.getY();
                downAtMs = System.currentTimeMillis();
                scheduleLongPress();
                break;

            case MotionEvent.ACTION_POINTER_DOWN:
                pointerCount = Math.max(pointerCount, event.getPointerCount());
                if (pointerCount >= 2) {
                    cancelLongPress();
                    longPressFired = false;
                    multiGestureUsed = true;
                    scrolling = false;
                    rightClickPending = true;
                    lastAvgY = (event.getY(0) + event.getY(1)) / 2f;
                }
                break;

            case MotionEvent.ACTION_MOVE: {
                if (pointerCount <= 1) {
                    float dx = event.getX() - lastX;
                    float dy = event.getY() - lastY;
                    if (dx != 0 || dy != 0) {
                        lastX = event.getX();
                        lastY = event.getY();
                        moveDistanceF += Math.abs(dx) + Math.abs(dy);
                        moved = true;
                        if (moveDistanceF > MAX_TAP_DIST) {
                            cancelLongPress();
                        }
                        client.sendMouse("move", null, false, dx * MOVE_SENSITIVITY,
                                dy * MOVE_SENSITIVITY);
                    }
                } else {
                    // два пальца: прокрутка (колёсико)
                    float avgY = (event.getY(0) + event.getY(1)) / 2f;
                    float avgDelta = avgY - lastAvgY;
                    if (!scrolling && Math.abs(avgDelta) > 3) {
                        scrolling = true;
                        rightClickPending = false;
                        lastAvgY = avgY;
                        break;
                    }
                    if (scrolling && Math.abs(avgDelta) >= 2) {
                        client.sendMouse("wheel", null, false, 0,
                                -avgDelta * SCROLL_SENSITIVITY);
                    }
                    lastAvgY = avgY;
                }
                break;
            }

            case MotionEvent.ACTION_POINTER_UP: {
                pointerCount = Math.max(1, event.getPointerCount() - 1);
                if (pointerCount == 1) {
                    long dt = System.currentTimeMillis() - downAtMs;
                    if (multiGestureUsed && rightClickPending && !scrolling
                            && dt < 400) {
                        client.sendMouse("click", "right", false, 0, 0);
                    }
                    rightClickPending = false;
                    scrolling = false;
                    int up = event.getActionIndex();
                    int remaining = (up == 0) ? 1 : 0;
                    lastX = event.getX(remaining);
                    lastY = event.getY(remaining);
                }
                break;
            }

            case MotionEvent.ACTION_UP: {
                cancelLongPress();
                pointerCount = 0;
                long dt = System.currentTimeMillis() - downAtMs;
                boolean validTap = !longPressFired && !multiGestureUsed && !moved
                        && dt < MAX_TAP_MS && moveDistanceF < MAX_TAP_DIST;
                if (validTap) {
                    long now = System.currentTimeMillis();
                    float tdx = event.getX() - lastTapX;
                    float tdy = event.getY() - lastTapY;
                    boolean isDouble = lastTapAtMs != 0
                            && now - lastTapAtMs <= DOUBLE_TAP_MS
                            && tdx * tdx + tdy * tdy
                            <= (float) DOUBLE_TAP_DIST * DOUBLE_TAP_DIST;
                    if (leftButtonHeld) {
                        // Касание во время удержания отпускает кнопку.
                        leftButtonHeld = false;
                        client.sendMouse("button", "left", false, 0, 0);
                        toast(getString(R.string.touchpad_drag_off));
                        lastTapAtMs = 0;
                    } else if (isDouble) {
                        // Двойное касание — зажать ЛКМ для перетаскивания.
                        cancelPendingClick();
                        leftButtonHeld = true;
                        client.sendMouse("button", "left", true, 0, 0);
                        toast(getString(R.string.touchpad_drag_on));
                        lastTapAtMs = 0;
                    } else {
                        // Одиночный клик отправляем с задержкой, чтобы не мешать
                        // распознаванию двойного касания.
                        lastTapAtMs = now;
                        lastTapX = event.getX();
                        lastTapY = event.getY();
                        scheduleSingleClick();
                    }
                }
                longPressFired = false;
                scrolling = false;
                multiGestureUsed = false;
                break;
            }

            case MotionEvent.ACTION_CANCEL:
                cancelLongPress();
                longPressFired = false;
                scrolling = false;
                multiGestureUsed = false;
                pointerCount = 0;
                break;
        }
        return true;
    }

    private void scheduleLongPress() {
        handler.postDelayed(longPressRunnable, LONG_PRESS_MS);
    }

    private void cancelLongPress() {
        handler.removeCallbacks(longPressRunnable);
    }

    /** Одиночный клик отправляется чуть позже — вдруг это начало двойного. */
    private void scheduleSingleClick() {
        cancelPendingClick();
        pendingSingleClick = () -> {
            pendingSingleClick = null;
            client.sendMouse("click", "left", false, 0, 0);
        };
        handler.postDelayed(pendingSingleClick, DOUBLE_TAP_MS);
    }

    private void cancelPendingClick() {
        if (pendingSingleClick != null) {
            handler.removeCallbacks(pendingSingleClick);
            pendingSingleClick = null;
        }
    }

    private void toast(String text) {
        Toast.makeText(this, text, Toast.LENGTH_SHORT).show();
    }

    @Override
    protected void onDestroy() {
        cancelPendingClick();
        cancelLongPress();
        if (leftButtonHeld && client != null) {
            leftButtonHeld = false;
            client.sendMouse("button", "left", false, 0, 0);
        }
        if (client != null) {
            client.setListener(null);
            client.stop();
        }
        super.onDestroy();
    }
}
