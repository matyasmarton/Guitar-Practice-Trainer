package dev.guitartrainer.app

import androidx.lifecycle.ViewModel
import dev.guitartrainer.ChallengeView
import dev.guitartrainer.Engine
import dev.guitartrainer.EngineEvent
import dev.guitartrainer.EngineListener
import dev.guitartrainer.FfiConfig
import dev.guitartrainer.FfiException
import dev.guitartrainer.FfiProgress
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow

/**
 * Immutable UI snapshot derived from engine events + polled progress.
 */
data class UiState(
    val promptDisplay: String = "",
    val promptKind: String = "",
    val targets: List<String> = emptyList(),
    val ordered: Boolean = false,
    val matched: Int = 0,
    val detectedNote: String? = null,
    val scorePassed: Int = 0,
    val scoreTotal: Int = 0,
    val timeFrac: Double = 0.0,
    val timeSecs: ULong = 0uL,
    val promptSecs: ULong = 0uL,
    val running: Boolean = false,
    val statusMessage: String? = null,
)

/**
 * Bridges [EngineListener] callbacks into a Compose-observable [StateFlow].
 *
 * The callback fires on the engine's driver thread; only the thread-safe
 * [MutableStateFlow] is touched here. Compose collects on the main dispatcher.
 */
class PracticeViewModel : ViewModel() {
    private val _ui = MutableStateFlow(UiState())
    val ui: StateFlow<UiState> = _ui.asStateFlow()

    private var engine: Engine? = null

    private val listener = object : EngineListener {
        override fun onEvent(ev: EngineEvent) {
            when (ev) {
                is EngineEvent.Prompt -> applyPrompt(ev.v1)
                is EngineEvent.DetectedNote -> _ui.value = _ui.value.copy(
                    detectedNote = ev.v1 ?: "—"
                )
                is EngineEvent.Matched -> _ui.value = _ui.value.copy(
                    matched = (_ui.value.matched + 1).coerceAtMost(ev.total.toInt())
                )
                is EngineEvent.Passed ->
                    _ui.value = _ui.value.copy(matched = _ui.value.targets.size)
                is EngineEvent.Timeout -> _ui.value = _ui.value.copy(matched = 0)
                is EngineEvent.Score -> _ui.value = _ui.value.copy(
                    scorePassed = ev.passed.toInt(),
                    scoreTotal = ev.total.toInt(),
                )
            }
        }
    }

    private fun applyPrompt(v: ChallengeView) {
        _ui.value = _ui.value.copy(
            promptDisplay = v.display,
            promptKind = v.kind,
            targets = v.targets,
            ordered = v.ordered,
            matched = 0,
        )
    }

    /** Build the engine with [config] and start practice. Safe to call once. */
    fun start(config: FfiConfig) {
        if (engine != null) return
        try {
            val eng = Native.createEngine(config, listener)
            eng.ffiStart()
            engine = eng
            _ui.value = _ui.value.copy(running = true, statusMessage = null)
        } catch (e: FfiException) {
            _ui.value = _ui.value.copy(statusMessage = "mic error: ${e.message}")
        }
    }

    fun stop() {
        engine?.ffiStop()
        engine = null
        _ui.value = _ui.value.copy(running = false)
    }

    fun skip() {
        engine?.ffiSkip()
    }

    /** Poll the timer progress (call from a periodic effect). */
    fun pollProgress() {
        val eng = engine ?: return
        val p: FfiProgress = eng.ffiProgress()
        _ui.value = _ui.value.copy(
            timeFrac = p.frac,
            timeSecs = p.secs,
            promptSecs = p.promptSecs,
        )
    }

    override fun onCleared() {
        stop()
    }
}