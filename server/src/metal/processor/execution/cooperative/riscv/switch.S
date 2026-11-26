.section .text.execution,"ax"
.balign 4
.globl limen_switch_context
.type limen_switch_context, @function
limen_switch_context:
    sd ra, {resume}(a0)
    sd sp, {stack}(a0)
    sd s0, {preserved}+0(a0)
    sd s1, {preserved}+8(a0)
    sd s2, {preserved}+16(a0)
    sd s3, {preserved}+24(a0)
    sd s4, {preserved}+32(a0)
    sd s5, {preserved}+40(a0)
    sd s6, {preserved}+48(a0)
    sd s7, {preserved}+56(a0)
    sd s8, {preserved}+64(a0)
    sd s9, {preserved}+72(a0)
    sd s10, {preserved}+80(a0)
    sd s11, {preserved}+88(a0)

    ld ra, {resume}(a1)
    ld sp, {stack}(a1)
    ld s0, {preserved}+0(a1)
    ld s1, {preserved}+8(a1)
    ld s2, {preserved}+16(a1)
    ld s3, {preserved}+24(a1)
    ld s4, {preserved}+32(a1)
    ld s5, {preserved}+40(a1)
    ld s6, {preserved}+48(a1)
    ld s7, {preserved}+56(a1)
    ld s8, {preserved}+64(a1)
    ld s9, {preserved}+72(a1)
    ld s10, {preserved}+80(a1)
    ld s11, {preserved}+88(a1)
    mv a0, a2
    ret
.size limen_switch_context, .-limen_switch_context
