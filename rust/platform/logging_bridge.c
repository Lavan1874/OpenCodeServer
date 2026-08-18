#include <os/log.h>
#include <pthread.h>
#include <stdint.h>

static os_log_t agent_log;
static pthread_once_t agent_log_once = PTHREAD_ONCE_INIT;

static void initialize_agent_log(void) {
    agent_log = os_log_create("ai.opencode.server", "agent");
}

void ocs_unified_log(uint8_t level, const char *message) {
    pthread_once(&agent_log_once, initialize_agent_log);

    os_log_type_t type = OS_LOG_TYPE_DEFAULT;
    switch (level) {
    case 1:
        type = OS_LOG_TYPE_INFO;
        break;
    case 2:
        type = OS_LOG_TYPE_ERROR;
        break;
    case 3:
        type = OS_LOG_TYPE_FAULT;
        break;
    default:
        break;
    }

    os_log_with_type(agent_log, type, "%{public}s", message);
}

