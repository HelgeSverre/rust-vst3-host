> / [VST Home](../../) / [Technical Documentation](../Index.html)
>
> [VST Module Architecture](#vst-module-architecture)
> ===================================================

**On this page:**

* [Introduction](#introduction)
* [Interfaces](#interfaces)
    * [FUnknown](#funknown)
    * [IID/CID](#iidcid)
    * [Direction](#direction)
    * [Versioning and inheritance](#versioning-and-inheritance)
    * [COM Compatibility](#com-compatibility)
* [Plug-ins](#plug-ins)
    * [Module Factory](#module-factory)
    * [Locations](#locations)
    * [Categories](#categories)
    * [IPluginBase](#ipluginbase)
    * [Purpose-specific interfaces](#purpose-specific-interfaces)
    * [Unicode](#unicode)
        * [Plug-ins for Unicode hosts](#plug-ins-for-unicode-hosts)
        * [Migrating from non-Unicode to Unicode](#migrating-from-non-unicode-to-unicode)
* [SDK backward compatibility](#sdk-backward-compatibility)

**Related pages:**

* [How the host will load a VST-MA based Plug-in](Loading.html)
* [How to derive a class from an interface](Derive+From+Interface.html)
* [Interface Versions and Inheritance](Interface+Versions+and+Inheritance.html)
* [VST 3 API Documentation](../API+Documentation/Index.html)

* * *

[Introduction](#introduction)
-----------------------------

**VST-MA** is a component model system which is used in all [Steinberg](https://www.steinberg.net/) host applications as
the basic layer for plug-in support.

It is object-oriented, cross-platform and (almost) compiler-independent.  
The basics are very much like [Microsoft® COM](https://en.wikipedia.org/wiki/Component_Object_Model), so if you are
familiar with this technology, understanding **VST-MA** should be quite easy.

**VST-MA** is provided in C++ only. Interfaces in C++ are expressed as pure virtual class (which is a class with nothing
but abstract methods). Unlike COM there is no support for C or other languages yet - simply because there has been no
need for this so far. But all **VST-MA** interfaces can be transformed into different representations in case this
should be inevitable some day.  
It is currently available for Windows, macOS X and Linux.

The C++ files belonging to **VST-MA** are located in the following folders:

* _pluginterfaces/base_
* _pluginterfaces/gui_

**Note**: The name '**VST Module Architecture**' has only little relation to the 'Virtual Studio Technology' itself.  
It describes the basic layer for any plug-in category supported in [Steinberg](https://www.steinberg.net/) hosts. *
*VST-MA** existed long before it was used as a base for **VST 3** itself.

[Interfaces](#interfaces)
-------------------------

### [FUnknown](#funknown)

[Steinberg::FUnknown](https://steinbergmedia.github.io/vst3_doc/base/classSteinberg_1_1FUnknown.html) is the basic
interface of **VST-MA**. All other interfaces are directly or indirectly derived from it.

### [IID/CID](#iidcid)

Each interface has a unique identifier (IID) of
type [Steinberg::FUID](https://steinbergmedia.github.io/vst3_doc/base/classSteinberg_1_1FUID.html). It is used to
retrieve a new interface from another
one ([Steinberg::FUnknown::queryInterface](https://steinbergmedia.github.io/vst3_doc/base/classSteinberg_1_1FUnknown.html#a4199134d0669bfa92b7419dac14c01a7)).
It is important to understand the difference between interface identifier and component identifier.  
A component-ID or class-ID (CID) is used to identify a concrete implementation class and is usually passed to a class
factory in order to create the corresponding component.  
So a lot of different classes (with different class identifiers) can implement the same interfaces.

### [Direction](#direction)

An interface may have a **direction**, meaning that the interface is expected to be implemented either in the plug-in or
in the host. The nature of an interface is documented in the following way:

* **\[host imp\]**: the host implements the interface
* **\[plug imp\]**: the plug-in implements the interface

When neither of these is specified, the interface can be used in both ways.

### [Versioning and inheritance](#versioning-and-inheritance)

Unlike C++ classes, interfaces do not use inheritance to express specializations of objects. Inheritance is used for
versioning only. One of the strict rules is that once an interface has been released, it must never change again. Adding
new functionality to an interface requires a new version (usually an ordinal number is added to its name in this case,
for
example, [Steinberg:: IPluginFactory3](https://steinbergmedia.github.io/vst3_doc/base/classSteinberg_1_1IPluginFactory3.html)
adds new features
to [Steinberg:: IPluginFactory2](https://steinbergmedia.github.io/vst3_doc/base/classSteinberg_1_1IPluginFactory2.html)).  
A new version inherits the old version(s) of the interface, so the old and the new methods are combined in one
interface. This is why specializations need to be modeled as separate interfaces! If a specialized interface were to
inherit from the basic interface as well, an implementation class that needs to implement all of these interfaces would
inherit the base interface twice, causing the compiler to run into ambiguities. So the specialization relation to a
basic interface can only be expressed in the documentation.

> * ISpecialInterface \[**extends** IBaseInterface\] => means IBaseInterface::queryInterface (ISpecialInterface::
    iid, ...) can be used to retrieve the derived interface.

You can find some example code here: [Interface Versions and Inheritance](Interface+Versions+and+Inheritance.html).

### [COM Compatibility](#com-compatibility)

The first layer of **VST-MA** is binary-compatible to **COM**.
The [Vtable](https://en.wikipedia.org/wiki/Virtual_method_table) and interface identifier
of [Steinberg::FUnknown](https://steinbergmedia.github.io/vst3_doc/base/classSteinberg_1_1FUnknown.html) match with the
corresponding COM interface [IUnknown](https://en.wikipedia.org/wiki/IUnknown). The main difference is the organization
and creation of components/plug-ins by a host application. VST-MA does not require any Microsoft® COM source file. You
can find information about **COM** on pages like:

* [https://docs.microsoft.com/en-us/windows/win32/learnwin32/what-is-a-com-interface-](https://docs.microsoft.com/en-us/windows/win32/learnwin32/what-is-a-com-interface-)

Basic Interfaces

* [Steinberg::FUnknown](https://steinbergmedia.github.io/vst3_doc/base/classSteinberg_1_1FUnknown.html)
* [Steinberg:: IPluginBase](https://steinbergmedia.github.io/vst3_doc/base/classSteinberg_1_1IPluginBase.html)
* [Steinberg:: IPluginFactory](https://steinbergmedia.github.io/vst3_doc/base/classSteinberg_1_1IPluginFactory.html)

Helper Classes

* [Steinberg::FUID](https://steinbergmedia.github.io/vst3_doc/base/classSteinberg_1_1FUID.html)
* [Steinberg::FUnknownPtr](https://steinbergmedia.github.io/vst3_doc/base/classSteinberg_1_1FUnknownPtr.html)

See also "[How to derive a class from an interface](Derive+From+Interface.html)".

[Plug-ins](#plug-ins)
---------------------

### [Module Factory](#module-factory)

A module (Windows: Dynamic Link Library, macOS: Mach-O Bundle, Linux: package) contains the implementation of one or
more components (e.g. VST 3 effects). A **VST-MA** module must contain a class factory where meta-data and
create-methods for the components are registered.  
The host has access to this factory through
the [Steinberg:: IPluginFactory](https://steinbergmedia.github.io/vst3_doc/base/classSteinberg_1_1IPluginFactory.html)
interface. This is the anchor point for the module and it is realized as a C-style export function
named [GetPluginFactory](https://steinbergmedia.github.io/vst3_doc/base/group__pluginBase.html#ga843ac97a36dfc717dadaa7192c7e8330).
You can find an export definition file in the SDK - _public.sdk/source/main/winexport.def_ (
_public.sdk/source/main/macexport.exp_) which can be used to export this function or you could use the macro
SMTG\_EXPORT\_SYMBOL directly in cpp file (check _public.sdk/source/main/dllmain.cpp_ for example).  
[GetPluginFactory](https://steinbergmedia.github.io/vst3_doc/base/group__pluginBase.html#ga843ac97a36dfc717dadaa7192c7e8330)
is declared as follows:

`SMTG_EXPORT_SYMBOL IPluginFactory* PLUGIN_API GetPluginFactory ();`

In addition to the **GetPluginFactory** function the plug-in may has to export additional **entry/exit** functions
depending on the platform:

> **On Windows**  
> On _Windows_ the entry/exit functions are named **InitDll** / **ExitDll** and are _**optional!**_
>
> A Plug-in can export these functions and a host has to call the **InitDll** function directly after loading the
> plug-in via LoadLibrary and before calling **GetPluginFactory**. The **ExitDll** function must be called before the
> plug-in is unloaded via _FreeLibrary_ or on program termination without _FreeLibrary_.
>
> As _Windows_ already has this feature (
> see [DllMain](https://docs.microsoft.com/en-us/cpp/build/run-time-library-behavior?view=msvc-170) in Microsofts
> documentation) the above functions are optional.

> **On macOS**  
> On _macOS_ the entry/exit functions are named **bundleEntry** / **bundleExit** and are _**required!**_
>
> A plug-in must export these functions and a host has to call the **bundleEntry** function directly after loading the
> plug-in via _CFBundleLoadExecutable_ and before calling **GetPluginFactory**.
>
> The **bundleExit** function must be called before the plug-in is unloaded or on program termination.
>
> As _macOS_ does not have a standard entry function when loading a bundle the above functions are required and a host
> has to reject plug-ins not exporting these functions.

> **On Linux**  
> On _Linux_ the entry/exit functions are named **ModuleEntry** / **ModuleExit** and are _**required!**_
>
> A plug-in must export these functions and a host has to call the **ModuleEntry** function directly after loading the
> plug-In via dlopen and before calling **GetPluginFactory**.
>
> The **ModuleExit** function must be called before the plug-in is unloaded via dlclose or on program termination.
>
> As _Linux_ does not have a standard entry function when loading a dynamic library, the above functions are required
> and a host has to reject plug-ins not exporting these functions.
>
> The entry function is intended for providing the plug-in with the platform specific instance handle which are needed
> for many platform APIs.
>
> Plug-in developers should use these functions instead of using platform functions to get the instance handle.

Here an example when using def/exp files instead of SMTG\_EXPORT\_SYMBOL:

**winexport.def file on Windows**

`EXPORTS GetPluginFactory InitDll ExitDll`

**macexport.exp file on mac**

`_GetPluginFactory _bundleEntry _bundleExit`

### [Locations](#locations)

Component modules do not require registration like **DirectX**. The host application expects component modules to be
located in predefined folders of the file system. These folders and their subfolders are scanned for **VST-MA** modules
during application startup. Each folder serves a special purpose:

* The application's _Components_ subfolder (e.g. _"C:\\Program Files\\Steinberg\\Cubase 12\\Components"_) is used for
  components tightly bound to the application. No other application should use it.
* Components that are shared between all [Steinberg](https://www.steinberg.net/) hosts are located at:
    * Win: _"/Program Files/Common Files/Steinberg/Shared Components"_
    * Mac: _"/Library/Application Support/Steinberg/Components/"_
* For special purpose plug-in types, additional locations are defined. Please refer to the corresponding documentation
  to find out if additional folders are used and where to find them. For **VST 3**,
  see [VST 3 Locations/Format](../Locations+Format/Index.html).

### [Categories](#categories)

Any class that the factory can create is assigned to a category. It is this category that tells the host the purpose of
the class (and gives a hint of which interfaces it might implement).  
A class is also described with a name and it has a unique id.

* For example, the category for import/export filters is "**Project Filter**" and for **VST 3** audio plug-ins it is "*
  *Audio Module Class**".
* "Service" is a special category. The purpose of a class of this category is completely unknown to the host. It is
  loaded automatically during program start (provided that the user did not deactivate it).
* Since the factory can create any number of classes, one component library can contain multiple components of any type.

### [IPluginBase](#ipluginbase)

The entry-point interface for any component class
is [Steinberg:: IPluginBase](https://steinbergmedia.github.io/vst3_doc/base/classSteinberg_1_1IPluginBase.html). The
host uses this interface to initialize and to terminate the plug-in component. When the host initializes the plug-in, it
**has to pass** a so called context. This context contains any interface to the host that the plug-in will need to work.

### [Purpose-specific interfaces](#purpose-specific-interfaces)

Each plug-in category (VST 3 Effects, Project import/export Filters, Audio Codecs, etc...) defines its own set of
purpose-specific interfaces. These are not part of the basic **VST-MA** layer.

### [Unicode](#unicode)

Beginning with version 5 of Cubase and Nuendo, the internal structure of the host was modified for better support of
internationalization. Therefore, string handling was changed to utilize [Unicode](https://en.wikipedia.org/wiki/Unicode)
strings whenever strings are passed around. As a consequence, all the interfaces to plug-ins have changed from using
ASCI to Unicode strings for call and return parameters. So in turn, all plug-ins must be adapted to support Unicode.
This has major consequences in that:

* Unicode hosts (Cubase 5 or later) will only work with Unicode plug-ins. When loading a plug-in, a Unicode host checks
  the plug-in's type and will not load any non-Unicode plug-ins.
* Unicode plug-ins will **not** load in non-Unicode hosts. When loading, a Unicode plug-in requests information from the
  host and will not load if no Unicode host is detected. Therefore, if a plug-in is supposed to work with both older and
  newer hosts, it is best to provide two versions of the plug-in.

#### [Plug-ins for Unicode hosts](#plug-ins-for-unicode-hosts)

Writing plug-ins that are supposed to work only with Unicode hosts is easy. Use a current version of this SDK and
develop a plug-in as usual. Make sure that you only ever pass Unicode [UTF-16](https://en.wikipedia.org/wiki/UTF-16)
strings to interfaces that have strings as call parameters and also be prepared that strings returned by these
interfaces are always [UTF-16](https://en.wikipedia.org/wiki/UTF-16). Therefore, to make things easier, it is
recommended that Unicode strings are used throughout the plug-in's implementation, in order to avoid back and forth
conversions. Also, use the Steinberg::String and Steinberg::ConstString classes from the Base module, as they have been
designed to work universally on both Mac and Win.

#### [Migrating from non-Unicode to Unicode](#migrating-from-non-unicode-to-unicode)

In [Steinberg](https://www.steinberg.net/) SDKs released before Cubase 5, the interface functions were using pointers of
type _char_ for passing strings to and from the host. These have been changed now to using Steinberg's defined type
_tchar_ which is equivalent to _char16_, i.e. 16 bit character. In theory, there are many ways for representing 16 bit
characters, but we chose to use the industry standard [Unicode](https://en.wikipedia.org/wiki/Unicode), so strings are
expected to be encoded in [UTF-16](https://en.wikipedia.org/wiki/UTF-16).  
Accordingly, also the implementation of a plug-in needs to be adapted to deal correctly with Unicode-encoded strings, as
well as only ever passing Unicode strings to the host.

> ⓘ **Note**  
> Changing a function from using 8 bit to 16 bit character pointers may seem as only a minor modification, but in
> interface design this is a major intrusion, because an interface is a contract to the outside world that is never to be
> changed. Therefore, classes that are changed to use Unicode strings are distinguished and also receive a new unique
> class ID.

[SDK backward compatibility](#sdk-backward-compatibility)
---------------------------------------------------------

Even with the current SDK it is still possible to develop non-Unicode plug-ins. In the file
_pluginterfaces/base/ftypes.h_, the line _"#define UNICODE\_OFF"_ is commented out, but by uncommenting it you can
revert all interfaces to using single byte ASCII strings. Alternatively, you can also specify UNICODE\_OFF as a
preprocessor definition in your project file.  
Also, the plug-in's factory info now does not define the Unicode flag anymore, so a Unicode host sees the compiled
plug-in as non-Unicode. Also, when reverting to single byte strings the plug-in's implementation also has to be changed
to behave correctly.

> ⓘ **Note**  
> When undefining Unicode, the class IDs also revert to the old ones.

[](../../../pages/Technical+Documentation/API+Documentation/Index.html "Previous chapter")[](../../../pages/Technical+Documentation/VST+Module+Architecture/Loading.html "Next chapter")