cmake_minimum_required(VERSION 3.20)
project(profcast_examples CXX)

add_executable(cpp_workload cpp_workload.cpp)
target_compile_features(cpp_workload PRIVATE cxx_std_17)

# Optimized but debuggable: the sampler needs real symbols and frame pointers.
if(MSVC)
    # cl.exe or clang-cl: PDB via /Zi + /DEBUG.
    target_compile_options(cpp_workload PRIVATE /O2 /Zi /EHsc)
    target_link_options(cpp_workload PRIVATE /DEBUG)
else()
    target_compile_options(cpp_workload PRIVATE -O2 -g -fno-omit-frame-pointer)
    if(WIN32)
        # GNU-style clang targeting the MSVC ABI: emit a PDB rather than DWARF.
        target_compile_options(cpp_workload PRIVATE -gcodeview)
        target_link_options(cpp_workload PRIVATE -fuse-ld=lld -Wl,/DEBUG)
    endif()
endif()

# Land the binary directly in the build dir for every generator, so the justfile
# doesn't have to special-case multi-config layouts (build/Release/, etc.).
set_target_properties(cpp_workload PROPERTIES
    RUNTIME_OUTPUT_DIRECTORY "${CMAKE_BINARY_DIR}"
    RUNTIME_OUTPUT_DIRECTORY_RELEASE "${CMAKE_BINARY_DIR}"
    RUNTIME_OUTPUT_DIRECTORY_DEBUG "${CMAKE_BINARY_DIR}")
