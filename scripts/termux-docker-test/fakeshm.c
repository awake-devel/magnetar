// LD_PRELOAD shim used ONLY by scripts/termux-docker-test.sh.
//
// termux-docker has no /dev/ashmem (removed from mainline Linux in 5.18), so Termux's
// libandroid-shmem cannot back the small System V segment Postgres creates as a
// postmaster interlock. This emulates shmget/shmat/shmdt/shmctl with memfd within one
// process tree, which is all Postgres needs (backends are forked, never exec'd).
// Real phones still have /dev/ashmem for Termux (targetSdk 28), so this is never shipped.
#define _GNU_SOURCE
#include <errno.h>
#include <string.h>
#include <sys/ipc.h>
#include <sys/mman.h>
#include <sys/shm.h>
#include <sys/syscall.h>
#include <unistd.h>

#define MAX_SEGS 64

static struct {
    int used;
    key_t key;
    int fd;
    size_t size;
    int nattch;
} segs[MAX_SEGS];

static struct {
    const void *addr;
    size_t size;
    int id;
} maps[MAX_SEGS];

int shmget(key_t key, size_t size, int flags) {
    if (key != IPC_PRIVATE) {
        for (int i = 0; i < MAX_SEGS; i++) {
            if (segs[i].used && segs[i].key == key) {
                if ((flags & IPC_CREAT) && (flags & IPC_EXCL)) {
                    errno = EEXIST;
                    return -1;
                }
                return i;
            }
        }
    }
    if (!(flags & IPC_CREAT)) {
        errno = ENOENT;
        return -1;
    }
    for (int i = 0; i < MAX_SEGS; i++) {
        if (segs[i].used) continue;
        int fd = (int)syscall(SYS_memfd_create, "fakeshm", 0);
        if (fd < 0) return -1;
        if (ftruncate(fd, (off_t)size) != 0) {
            close(fd);
            return -1;
        }
        segs[i].used = 1;
        segs[i].key = key;
        segs[i].fd = fd;
        segs[i].size = size;
        segs[i].nattch = 0;
        return i;
    }
    errno = ENOSPC;
    return -1;
}

void *shmat(int id, const void *addr, int flags) {
    if (id < 0 || id >= MAX_SEGS || !segs[id].used) {
        errno = EINVAL;
        return (void *)-1;
    }
    int prot = PROT_READ | ((flags & SHM_RDONLY) ? 0 : PROT_WRITE);
    void *p = mmap((void *)addr, segs[id].size, prot, MAP_SHARED | (addr ? MAP_FIXED : 0),
                   segs[id].fd, 0);
    if (p == MAP_FAILED) return (void *)-1;
    for (int i = 0; i < MAX_SEGS; i++) {
        if (maps[i].addr == NULL) {
            maps[i].addr = p;
            maps[i].size = segs[id].size;
            maps[i].id = id;
            break;
        }
    }
    segs[id].nattch++;
    return p;
}

int shmdt(const void *addr) {
    for (int i = 0; i < MAX_SEGS; i++) {
        if (maps[i].addr == addr) {
            munmap((void *)addr, maps[i].size);
            if (segs[maps[i].id].nattch > 0) segs[maps[i].id].nattch--;
            maps[i].addr = NULL;
            return 0;
        }
    }
    errno = EINVAL;
    return -1;
}

int shmctl(int id, int cmd, struct shmid_ds *buf) {
    if (id < 0 || id >= MAX_SEGS || !segs[id].used) {
        errno = EINVAL;
        return -1;
    }
    switch (cmd) {
    case IPC_STAT:
        memset(buf, 0, sizeof(*buf));
        buf->shm_segsz = segs[id].size;
        buf->shm_nattch = (shmatt_t)segs[id].nattch;
        buf->shm_perm.mode = 0600;
        buf->shm_perm.uid = getuid();
        buf->shm_perm.cuid = getuid();
        return 0;
    case IPC_RMID:
        segs[id].used = 0;
        return 0;
    default:
        return 0;
    }
}

// Termux binaries may bind the prefixed names exported by libandroid-shmem.
int libandroid_shmget(key_t key, size_t size, int flags) { return shmget(key, size, flags); }
void *libandroid_shmat(int id, const void *addr, int flags) { return shmat(id, addr, flags); }
int libandroid_shmdt(const void *addr) { return shmdt(addr); }
int libandroid_shmctl(int id, int cmd, struct shmid_ds *buf) { return shmctl(id, cmd, buf); }
