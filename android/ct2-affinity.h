/*
 * Declares pthread_setaffinity_np for the CTranslate2 build.
 *
 * Bionic has it only from API 36 (pthread.h, __INTRODUCED_IN(36)) and
 * Scribe builds for 26, so the declaration is hidden and
 * ThreadPool::set_thread_affinity in CTranslate2 does not compile --
 * its only other branch is the one that throws "only supported in
 * Linux binaries", and Android defines __linux__.
 *
 * Nothing here reaches that function: affinity is set only when a
 * replica pool is given a core offset, and ct2rs gives none. It is
 * defined all the same, in vox_android, on top of the
 * sched_setaffinity Bionic has always had -- so if it is ever called it
 * will work rather than abort. This header is force-included into the
 * CTranslate2 build by `just _android-jni`.
 *
 * On API 36 and up Bionic declares it too, identically, which C++
 * allows.
 */
#pragma once
#include <pthread.h>
#include <sched.h>

#ifdef __cplusplus
extern "C" {
#endif

int pthread_setaffinity_np(pthread_t thread, size_t set_size, const cpu_set_t *set);

#ifdef __cplusplus
}
#endif
