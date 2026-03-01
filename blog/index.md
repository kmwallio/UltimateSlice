---
layout: default
title: Blog
permalink: /blog/
---

<div class="hero" style="padding-bottom: 40px;">
  <h1>Blog</h1>
  <p>Latest updates and stories from the UltimateSlice team.</p>
</div>

<section class="blog-feed">
  <div class="wrapper" style="max-width: 800px; margin: 0 auto;">
    {% for post in site.posts %}
      <article class="post-card">
        <h3><a href="{{ post.url | relative_url }}" style="text-decoration: none; color: inherit;">{{ post.title }}</a></h3>
        <p style="color: #86868b; font-size: 0.9rem; margin-bottom: 12px;">{{ post.date | date: "%B %d, %Y" }}</p>
        <p>{{ post.excerpt | strip_html | truncatewords: 30 }}</p>
        <p><a href="{{ post.url | relative_url }}" style="color: var(--accent-color); font-weight: 500;">Read more &rarr;</a></p>
      </article>
    {% else %}
      <p style="text-align: center;">No posts found yet. Check back soon!</p>
    {% endfor %}
  </div>
</section>
