package dev.guitartrainer.app

import android.Manifest
import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.result.contract.ActivityResultContracts
import androidx.activity.viewModels
import androidx.compose.animation.core.Animatable
import androidx.compose.animation.core.tween
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.remember
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.text.SpanStyle
import androidx.compose.ui.text.buildAnnotatedString
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.withStyle
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import kotlinx.coroutines.delay

class MainActivity : ComponentActivity() {
    private val vm: PracticeViewModel by viewModels()

    private val micPermission =
        registerForActivityResult(ActivityResultContracts.RequestPermission()) { granted ->
            if (granted) startPractice()
        }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        val theme = loadAppTheme(this)
        setContent {
            MaterialTheme {
                Surface(modifier = Modifier.fillMaxSize(), color = MaterialTheme.colorScheme.background) {
                    PracticeScreen(
                        vm = vm,
                        theme = theme,
                        onStart = { startPractice() },
                        onStop = { vm.stop() },
                        onSkip = { vm.skip() },
                    )
                }
            }
        }
    }

    private fun startPractice() {
        if (checkSelfPermission(Manifest.permission.RECORD_AUDIO) !=
            android.content.pm.PackageManager.PERMISSION_GRANTED
        ) {
            micPermission.launch(Manifest.permission.RECORD_AUDIO)
            return
        }
        val config = Native.loadConfig()
        vm.start(config)
    }

    override fun onPause() {
        super.onPause()
        vm.stop()
    }
}

@Composable
private fun PracticeScreen(
    vm: PracticeViewModel,
    theme: AppTheme,
    onStart: () -> Unit,
    onStop: () -> Unit,
    onSkip: () -> Unit,
) {
    val ui by vm.ui.collectAsState()

    LaunchedEffect(ui.running) {
        while (ui.running) {
            vm.pollProgress()
            delay(100)
        }
    }

    Scaffold { padding ->
        Column(
            modifier = Modifier
                .fillMaxSize()
                .padding(20.dp)
                .padding(padding),
            verticalArrangement = Arrangement.spacedBy(16.dp),
        ) {
            // Category label
            Text(
                text = ui.promptKind.uppercase(),
                style = MaterialTheme.typography.labelMedium,
                color = theme.accent,
                fontWeight = FontWeight.SemiBold,
                letterSpacing = 2.sp,
            )

            // Prompt display (the main thing the user plays)
            Text(
                text = ui.promptDisplay.ifEmpty { "Press Start to begin" },
                style = MaterialTheme.typography.headlineMedium,
                fontWeight = FontWeight.Bold,
                modifier = Modifier.fillMaxWidth(),
            )

            // Target note checklist: matched targets individually highlighted green.
            if (ui.targets.isNotEmpty()) {
                val separator = if (ui.ordered) "  ->  " else "   "
                val text = buildAnnotatedString {
                    append(if (ui.ordered) "[${ui.matchedIndices.size}/${ui.targets.size}]  " else "${ui.matchedIndices.size}/${ui.targets.size}  ")
                    ui.targets.forEachIndexed { i, name ->
                        if (i > 0) append(separator)
                        if (i in ui.matchedIndices) {
                            withStyle(SpanStyle(color = theme.success)) { append(name) }
                        } else {
                            append(name)
                        }
                    }
                }
                Text(
                    text = text,
                    style = MaterialTheme.typography.bodyLarge,
                    color = theme.secondary,
                    modifier = Modifier.fillMaxWidth(),
                )
            }

            Spacer(modifier = Modifier.height(8.dp))

            // Timer bar: fillMaxWidth so it doesn't eat vertical space
            LinearProgressIndicator(
                progress = { ui.timeFrac.toFloat().coerceIn(0f, 1f) },
                modifier = Modifier.fillMaxWidth().height(8.dp),
                color = if (ui.timeSecs <= 5uL && ui.running) theme.danger else MaterialTheme.colorScheme.primary,
            )
            Text(
                text = if (ui.running) "${ui.timeSecs}s / ${ui.promptSecs}s" else " ",
                style = MaterialTheme.typography.bodySmall,
                color = theme.secondary,
            )

            Spacer(modifier = Modifier.height(8.dp))

            // Detected note — scale-pop + color flash while the post-match cooldown is active.
            val detectedScale = remember { Animatable(1f) }
            LaunchedEffect(ui.cooldownUntilMs) {
                if (ui.cooldownUntilMs != null) {
                    detectedScale.snapTo(1f)
                    detectedScale.animateTo(1.25f, tween(200))
                    detectedScale.animateTo(1f, tween(200))
                }
            }
            val detectedColor = if (ui.cooldownUntilMs != null) theme.success else MaterialTheme.colorScheme.onSurface
            Text(
                text = "Detected: ${ui.detectedNote ?: "—"}",
                style = MaterialTheme.typography.titleMedium,
                fontWeight = FontWeight.Medium,
                color = detectedColor,
                modifier = Modifier
                    .fillMaxWidth()
                    .graphicsLayer(scaleX = detectedScale.value, scaleY = detectedScale.value),
            )

            // Score
            Text(
                text = "Score  ${ui.scorePassed} / ${ui.scoreTotal}",
                style = MaterialTheme.typography.titleSmall,
                color = theme.secondary,
                modifier = Modifier.fillMaxWidth(),
            )

            Spacer(modifier = Modifier.weight(1f))

            // Buttons at the bottom, always visible
            Row(
                modifier = Modifier.fillMaxWidth(),
                horizontalArrangement = Arrangement.spacedBy(12.dp),
            ) {
                Button(
                    onClick = if (ui.running) onStop else onStart,
                    modifier = Modifier.weight(1f),
                    colors = if (ui.running)
                        ButtonDefaults.buttonColors(containerColor = theme.danger)
                    else
                        ButtonDefaults.buttonColors(),
                ) {
                    Text(
                        text = if (ui.running) "Stop" else "Start",
                        fontSize = 18.sp,
                        fontWeight = FontWeight.Bold,
                    )
                }
                Button(
                    onClick = onSkip,
                    modifier = Modifier.weight(1f),
                ) {
                    Text("Skip", fontSize = 16.sp)
                }
            }

            ui.statusMessage?.let {
                Text(it, color = theme.danger)
            }
        }
    }
}