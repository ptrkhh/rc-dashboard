package dev.cdash.watch

// The pure rules of the watch, kept apart so a JVM unit test can pin them.

const val FIRST_DELAY_MS = 5_000L
private const val MAX_DELAY_MS = 300_000L

const val CHANNEL_REPLIES = "replies"
const val CHANNEL_DONE = "done"

/** The wait before the next reconnect: double the last one, at most 5 minutes. */
fun nextDelay(ms: Long): Long = minOf(ms * 2, MAX_DELAY_MS)

/** The JSON of a `data:` line of the event stream; comments and blank lines carry none. */
fun dataOf(line: String): String? =
    if (line.startsWith("data:")) line.removePrefix("data:").trim() else null

fun textFor(kind: String): String? = when (kind) {
    "done" -> "Finished"
    "error" -> "Stopped with an error"
    "ask" -> "Needs your answer"
    else -> null
}

/** Android sets importance per channel: answers and errors interrupt, finished turns do not. */
fun channelFor(kind: String): String = if (kind == "done") CHANNEL_DONE else CHANNEL_REPLIES
